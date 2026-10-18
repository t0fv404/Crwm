use std::collections::{HashMap, VecDeque};

use wayland_backend::client::ObjectId;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

use crate::river::river_pointer_binding_v1::RiverPointerBindingV1;
use crate::river::river_seat_v1::{Modifiers, RiverSeatV1};
use crate::river::river_window_manager_v1::RiverWindowManagerV1;
use crate::river::river_window_v1::{Edges, RiverWindowV1};
use crate::river::river_xkb_binding_v1::RiverXkbBindingV1;
use crate::river::river_xkb_bindings_v1::RiverXkbBindingsV1;
use crate::{AppData, Error, record_error};

use super::Window;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Action {
    None,
    SpawnFoot,
    Close,
    FocusNext,
    Move,
    Resize,
    Exit,
}

#[derive(Debug, Clone)]
pub(crate) enum SeatOp {
    None,
    Move {
        window_proxy: RiverWindowV1,
        start_x: i32,
        start_y: i32,
    },
    Resize {
        window_proxy: RiverWindowV1,
        start_x: i32,
        start_y: i32,
        start_width: i32,
        start_height: i32,
        edges: Edges,
    },
}

/// 一个 seat(键盘 + 指针)。
///
/// 字段是 `pub(crate)` 的:crate 根模块的 `WindowManager` 在 manage/render 序列里
/// 直接读取焦点、交互状态和 op 位移,拆分文件后不必再包一层访问方法。
#[derive(Debug)]
pub(crate) struct Seat {
    pub(crate) proxy: RiverSeatV1,
    pub(crate) new: bool,
    pub(crate) removed: bool,
    pub(crate) focused: Option<RiverWindowV1>,
    pub(crate) hovered: Option<RiverWindowV1>,
    pub(crate) interacted: Option<RiverWindowV1>,
    pub(crate) xkb_bindings: HashMap<ObjectId, XkbBinding>,
    pub(crate) pointer_bindings: HashMap<ObjectId, PointerBinding>,
    pending_action: Action,
    pub(crate) op: SeatOp,
    pub(crate) op_dx: i32,
    pub(crate) op_dy: i32,
    pub(crate) op_release: bool,
}

#[derive(Debug)]
pub(crate) struct XkbBinding {
    pub(crate) proxy: RiverXkbBindingV1,
    action: Action,
}

#[derive(Debug)]
pub(crate) struct PointerBinding {
    pub(crate) proxy: RiverPointerBindingV1,
    action: Action,
}

impl Seat {
    pub(crate) fn new(proxy: RiverSeatV1) -> Self {
        Self {
            proxy,
            new: true,
            removed: false,
            focused: None,
            hovered: None,
            interacted: None,
            xkb_bindings: HashMap::new(),
            pointer_bindings: HashMap::new(),
            pending_action: Action::None,
            op: SeatOp::None,
            op_dx: 0,
            op_dy: 0,
            op_release: false,
        }
    }

    pub(crate) fn create_xkb_binding(
        &mut self,
        river_xkb: &RiverXkbBindingsV1,
        qh: &QueueHandle<AppData>,
        mods: Modifiers,
        keysym: u32,
        action: Action,
    ) {
        let proxy = river_xkb.get_xkb_binding(&self.proxy, keysym, mods, qh, self.proxy.id());
        proxy.enable();
        let binding = XkbBinding { proxy, action };
        self.xkb_bindings.insert(binding.proxy.id(), binding);
    }

    pub(crate) fn create_pointer_binding(
        &mut self,
        qh: &QueueHandle<AppData>,
        mods: Modifiers,
        button: u32,
        action: Action,
    ) {
        let proxy = self
            .proxy
            .get_pointer_binding(button, mods, qh, self.proxy.id());
        proxy.enable();
        let binding = PointerBinding { proxy, action };
        self.pointer_bindings.insert(binding.proxy.id(), binding);
    }

    pub(crate) fn do_action(
        &mut self,
        windows: &mut VecDeque<Window>,
        wm_proxy: &RiverWindowManagerV1,
    ) {
        match self.pending_action {
            Action::None => {}
            // Don't pass WAYLAND_DEBUG on to children, the added noise makes
            // debugging the window manager itself impractical.
            Action::SpawnFoot => {
                if let Err(e) = std::process::Command::new("foot")
                    .env_remove("WAYLAND_DEBUG")
                    .spawn()
                {
                    // 起不来一个进程不该让整个 WM 退出,记日志就够。
                    eprintln!("Failed to spawn foot: {e}");
                }
            }
            Action::Close => {
                if let Some(window_proxy) = self.focused.as_ref() {
                    window_proxy.close();
                }
            }
            Action::FocusNext => {
                if !windows.is_empty() {
                    windows.rotate_left(1);
                    self.focus_top(windows);
                }
            }
            Action::Move => {
                if let (Some(window_proxy), SeatOp::None) = (self.hovered.as_ref(), &self.op) {
                    // 悬停的窗口可能已经关掉(pointer_leave 还没到),跳过即可,不能 panic。
                    if let Some(window) = windows.iter().find(|w| &w.proxy == window_proxy) {
                        self.pointer_move(window);
                    }
                }
            }
            Action::Resize => {
                if let (Some(window_proxy), SeatOp::None) = (self.hovered.as_ref(), &self.op) {
                    // 同上:窗口消失属于正常竞态。
                    if let Some(window) = windows.iter().find(|w| &w.proxy == window_proxy) {
                        self.pointer_resize(window, Edges::Bottom.union(Edges::Right));
                    }
                }
            }
            Action::Exit => wm_proxy.exit_session(),
        }
        self.pending_action = Action::None;
    }

    pub(crate) fn op_end(&mut self) {
        if let SeatOp::Resize { window_proxy, .. } = &self.op {
            window_proxy.inform_resize_end();
        }
        self.proxy.op_end();
        self.op = SeatOp::None;
    }

    pub(crate) fn op_manage(&mut self) {
        match &self.op {
            SeatOp::None | SeatOp::Move { .. } => {}
            SeatOp::Resize {
                window_proxy,
                start_width,
                start_height,
                edges,
                ..
            } => {
                let (mut width, mut height) = (*start_width, *start_height);
                if edges.contains(Edges::Left) {
                    width -= self.op_dx;
                }
                if edges.contains(Edges::Right) {
                    width += self.op_dx;
                }
                if edges.contains(Edges::Top) {
                    height -= self.op_dy;
                }
                if edges.contains(Edges::Bottom) {
                    height += self.op_dy;
                }
                window_proxy.propose_dimensions(width.max(1), height.max(1));
            }
        }
    }

    pub(crate) fn focus_top(&mut self, windows: &VecDeque<Window>) {
        let Some(window) = windows.back() else {
            // 没有窗口:只有确实还持有焦点时才需要清一次,
            // 否则每个 manage 序列都会白发一条 clear_focus。
            if self.focused.take().is_some() {
                self.proxy.clear_focus();
            }
            return;
        };

        // 焦点已经在这个窗口上时不再重复发送 focus_window/place_top。
        // 栈顶窗口必然是上一次 place_top 调过的那个,重复抬升没有意义。
        if self.focused.as_ref() == Some(&window.proxy) {
            return;
        }

        self.proxy.focus_window(&window.proxy);
        window.node.place_top();
        self.focused = Some(window.proxy.clone());
    }

    pub(crate) fn pointer_move(&mut self, window: &Window) {
        self.interacted = Some(window.proxy.clone());
        self.proxy.op_start_pointer();
        self.op = SeatOp::Move {
            window_proxy: window.proxy.clone(),
            start_x: window.x,
            start_y: window.y,
        };
        self.op_dx = 0;
        self.op_dy = 0;
    }

    pub(crate) fn pointer_resize(&mut self, window: &Window, edges: Edges) {
        self.interacted = Some(window.proxy.clone());
        self.proxy.op_start_pointer();
        window.proxy.inform_resize_start();
        self.op = SeatOp::Resize {
            window_proxy: window.proxy.clone(),
            start_x: window.x,
            start_y: window.y,
            start_width: window.width,
            start_height: window.height,
            edges,
        };
        self.op_dx = 0;
        self.op_dy = 0;
    }
}

impl Dispatch<RiverSeatV1, ()> for AppData {
    fn event(
        state: &mut Self,
        proxy: &RiverSeatV1,
        event: <RiverSeatV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use crate::river::river_seat_v1::Event;

        // seat 已经被销毁时事件可能迟到:记录错误并跳过,而不是 panic。
        let Some(seat) = state.wm.seats.get_mut(&proxy.id()) else {
            record_error(&mut state.err, Error::MissingSeat);
            return;
        };
        match event {
            Event::Removed => seat.removed = true,
            Event::PointerEnter { window } => seat.hovered = Some(window),
            Event::PointerLeave => seat.hovered = None,
            Event::WindowInteraction { window } => seat.interacted = Some(window),
            Event::OpDelta { dx, dy } => (seat.op_dx, seat.op_dy) = (dx, dy),
            Event::OpRelease => seat.op_release = true,
            _ => {}
        }
    }
}

impl Dispatch<RiverXkbBindingV1, ObjectId> for AppData {
    fn event(
        state: &mut Self,
        proxy: &RiverXkbBindingV1,
        event: <RiverXkbBindingV1 as Proxy>::Event,
        data: &ObjectId,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use crate::river::river_xkb_binding_v1::Event;

        // 绑定对象属于某个 seat;seat 没了说明事件迟到,直接跳过。
        let Some(seat) = state.wm.seats.get_mut(data) else {
            record_error(&mut state.err, Error::MissingSeat);
            return;
        };
        let Some(binding) = seat.xkb_bindings.get(&proxy.id()) else {
            // 这里 seat 的可变借用还活着,所以只借用 state.err 这个字段来记录错误。
            record_error(&mut state.err, Error::MissingBinding);
            return;
        };
        if let Event::Pressed = event {
            seat.pending_action = binding.action;
        }
    }
}

impl Dispatch<RiverPointerBindingV1, ObjectId> for AppData {
    fn event(
        state: &mut Self,
        proxy: &RiverPointerBindingV1,
        event: <RiverPointerBindingV1 as Proxy>::Event,
        data: &ObjectId,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use crate::river::river_pointer_binding_v1::Event;

        // 同上:seat 或绑定对象可能已经不存在。
        let Some(seat) = state.wm.seats.get_mut(data) else {
            record_error(&mut state.err, Error::MissingSeat);
            return;
        };
        let Some(binding) = seat.pointer_bindings.get(&proxy.id()) else {
            record_error(&mut state.err, Error::MissingBinding);
            return;
        };
        if let Event::Pressed = event {
            seat.pending_action = binding.action;
        }
    }
}
