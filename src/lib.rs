use std::collections::{HashMap, VecDeque};
use std::fmt;

use wayland_backend::client::ObjectId;
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry,
};

use crate::river::{
    river_node_v1::RiverNodeV1,
    river_output_v1::RiverOutputV1,
    river_seat_v1::{Modifiers, RiverSeatV1},
    river_window_manager_v1::RiverWindowManagerV1,
    river_window_v1::{Edges, RiverWindowV1},
    river_xkb_bindings_v1::RiverXkbBindingsV1,
};

mod river {
    pub extern crate wayland_client;
    pub use wayland_client::protocol::*;

    mod interfaces {
        pub(super) mod rwm {
            pub use wayland_client::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("./protocol/river-window-management-v1.xml");
        }

        pub(super) mod rxkb {
            use super::rwm::*;
            wayland_scanner::generate_interfaces!("./protocol/river-xkb-bindings-v1.xml");
        }
    }

    use self::interfaces::rwm::*;
    use self::interfaces::rxkb::*;
    wayland_scanner::generate_client_code!("./protocol/river-window-management-v1.xml");
    wayland_scanner::generate_client_code!("./protocol/river-xkb-bindings-v1.xml");
}

mod wm;
use wm::{Action, Output, Seat, SeatOp, Window};

#[derive(Debug)]
pub enum Error {
    /// 已经有别的 WM 在运行。
    AlreadyRunning,
    /// 事件里的 seat 不在 `seats` 表中。
    MissingSeat,
    /// 事件里的 output 不在 `outputs` 表中。
    MissingOutput,
    /// 事件里的按键 / 指针绑定不在绑定表中。
    MissingBinding,
    /// river_window_v1 传来的 edges 违反协议。
    InvalidEdges,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::AlreadyRunning => f.write_str("another window manager is already running"),
            Error::MissingSeat => f.write_str("seat for event not found"),
            Error::MissingOutput => f.write_str("output for event not found"),
            Error::MissingBinding => f.write_str("binding for event not found"),
            Error::InvalidEdges => f.write_str("river_window_v1 sent invalid edges"),
        }
    }
}

impl std::error::Error for Error {}

/// 记录回调中的第一个错误(只保留根因,后面的错误不覆盖)。
///
/// 刻意做成接收 `&mut Option<Error>` 的自由函数而不是 `AppData` 的方法:
/// 这样只借用 `state.err` 一个字段,在 `state.wm` 正被可变借用时也能调用。
pub(crate) fn record_error(slot: &mut Option<Error>, err: Error) {
    if slot.is_none() {
        *slot = Some(err);
    }
}

#[derive(Debug)]
pub(crate) struct AppData {
    #[allow(dead_code)]
    river_wm: RiverWindowManagerV1,
    river_xkb: RiverXkbBindingsV1,
    wm: WindowManager,
    err: Option<Error>,
    finished: bool,
}

#[derive(Debug, Default)]
struct WindowManager {
    windows: VecDeque<Window>,
    outputs: HashMap<ObjectId, Output>,
    seats: HashMap<ObjectId, Seat>,
}

impl WindowManager {
    /// 处理一次 manage 序列。
    ///
    /// 即使中途出错,也要先把 `manage_finish` 发出去再返回错误:
    /// 少了它 river 会一直等下去,最终按 unresponsive 错误处理。
    fn handle_manage_start(
        &mut self,
        proxy: &RiverWindowManagerV1,
        river_xkb: &RiverXkbBindingsV1,
        qh: &QueueHandle<AppData>,
    ) -> Result<(), Error> {
        self.remove_outputs();
        self.remove_windows();
        self.remove_seats();
        self.init_new_windows();
        self.init_new_seats(river_xkb, qh);
        let result = self.manage_windows();
        self.manage_seats(proxy);
        proxy.manage_finish();
        result
    }

    fn handle_render_start(&mut self, proxy: &RiverWindowManagerV1) {
        for seat in &mut self.seats.values_mut() {
            // 没有进行中的交互操作就直接跳过:render 序列非常频繁,
            // 而窗口表的线性查找没必要为每个空闲 seat 都做一遍。
            if let SeatOp::None = seat.op {
                continue;
            }
            match &seat.op {
                SeatOp::None => {}
                SeatOp::Move {
                    window_proxy,
                    start_x,
                    start_y,
                } => {
                    if let Some(window) = self
                        .windows
                        .iter_mut()
                        .find(|window| &window.proxy == window_proxy)
                    {
                        window.set_position(start_x + seat.op_dx, start_y + seat.op_dy);
                    }
                }
                SeatOp::Resize {
                    window_proxy,
                    start_x,
                    start_y,
                    start_width,
                    start_height,
                    edges,
                } => {
                    if let Some(window) = self
                        .windows
                        .iter_mut()
                        .find(|window| &window.proxy == window_proxy)
                    {
                        let (mut x, mut y) = (*start_x, *start_y);
                        if edges.contains(Edges::Left) {
                            x += start_width - window.width;
                        }
                        if edges.contains(Edges::Top) {
                            y += start_height - window.height;
                        }
                        window.set_position(x, y);
                    }
                }
            }
        }

        proxy.render_finish();
    }

    fn remove_outputs(&mut self) {
        self.outputs.retain(|_, output| {
            if output.removed {
                output.proxy.destroy();
                return false;
            }
            true
        });
    }

    fn remove_windows(&mut self) {
        let seats = &mut self.seats;
        self.windows.retain_mut(|window| {
            if !window.closed {
                return true;
            }
            for seat in seats.values_mut() {
                if let SeatOp::Move { window_proxy, .. } | SeatOp::Resize { window_proxy, .. } =
                    &seat.op
                    && window_proxy == &window.proxy
                {
                    seat.op_end();
                }
            }
            false
        });
    }

    fn remove_seats(&mut self) {
        self.seats.retain(|_, seat| {
            if seat.removed {
                seat.xkb_bindings
                    .values_mut()
                    .for_each(|binding| binding.proxy.destroy());
                seat.pointer_bindings
                    .values_mut()
                    .for_each(|binding| binding.proxy.destroy());
                seat.proxy.destroy();
                return false;
            }
            true
        });
    }

    fn init_new_windows(&mut self) {
        for window in self.windows.iter_mut().filter(|w| w.new) {
            window.set_position(window.x, window.y);
            window.proxy.propose_dimensions(window.width, window.height);
            window.new = false;
        }
    }

    // TODO 把绑定鼠标和键盘改成可配置
    fn init_new_seats(&mut self, river_xkb: &RiverXkbBindingsV1, qh: &QueueHandle<AppData>) {
        // See xkbcommon/xkbcommon-keysyms.h
        const SPACE: u32 = 0x20;
        const N: u32 = 0x6e;
        const Q: u32 = 0x71;
        const ESC: u32 = 0xff1b;
        // See linux/input-event-codes.h
        const BTN_LEFT: u32 = 0x110;
        const BTN_RIGHT: u32 = 0x111;
        let mods = Modifiers::Mod4;

        for seat in self.seats.values_mut() {
            if seat.new {
                seat.create_xkb_binding(river_xkb, qh, mods, SPACE, Action::SpawnFoot);
                seat.create_xkb_binding(river_xkb, qh, mods, Q, Action::Close);
                seat.create_xkb_binding(river_xkb, qh, mods, N, Action::FocusNext);
                seat.create_xkb_binding(river_xkb, qh, mods, ESC, Action::Exit);
                seat.create_pointer_binding(qh, mods, BTN_LEFT, Action::Move);
                seat.create_pointer_binding(qh, mods, BTN_RIGHT, Action::Resize);
                seat.new = false;
            }
        }
    }

    fn manage_windows(&mut self) -> Result<(), Error> {
        for window in self.windows.iter_mut() {
            if let Some(seat_proxy) = window.pointer_move_requested.take() {
                let seat = self
                    .seats
                    .get_mut(&seat_proxy.id())
                    .ok_or(Error::MissingSeat)?;
                seat.pointer_move(window);
            }
            if let Some(seat_proxy) = window.pointer_resize_requested.take() {
                let seat = self
                    .seats
                    .get_mut(&seat_proxy.id())
                    .ok_or(Error::MissingSeat)?;
                seat.pointer_resize(window, window.pointer_resize_requested_edges);
            }
        }
        Ok(())
    }

    fn manage_seats(&mut self, wm_proxy: &RiverWindowManagerV1) {
        for seat in self.seats.values_mut() {
            if let Some(window_proxy) = seat.interacted.take() {
                // 交互事件可能指向本轮已经被 remove_windows 删掉的窗口,
                // 这是正常的竞态:跳过抬升即可,不能 panic。
                let index = self
                    .windows
                    .iter()
                    .position(|window| window.proxy == window_proxy);
                if let Some(index) = index
                    && let Some(window) = self.windows.remove(index)
                {
                    self.windows.push_back(window);
                }
            }
            seat.focus_top(&self.windows);
            seat.do_action(&mut self.windows, wm_proxy);
            if seat.op_release {
                seat.op_end();
                seat.op_release = false;
            } else {
                seat.op_manage();
            }
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for AppData {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: <wl_registry::WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<RiverWindowManagerV1, ()> for AppData {
    fn event(
        state: &mut Self,
        proxy: &RiverWindowManagerV1,
        event: <RiverWindowManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use river::river_window_manager_v1::Event;
        match event {
            Event::Seat { id } => {
                state.wm.seats.insert(id.id(), Seat::new(id));
            }
            Event::Output { id } => {
                state.wm.outputs.insert(id.id(), Output::new(id));
            }
            Event::Window { id } => state.wm.windows.push_back(Window::new(id, qh)),
            // 回调不能返回 Result:出错就记录到 state.err,由主循环交给 main。
            Event::ManageStart => {
                if let Err(err) = state.wm.handle_manage_start(proxy, &state.river_xkb, qh) {
                    record_error(&mut state.err, err);
                }
            }
            Event::RenderStart => state.wm.handle_render_start(proxy),
            Event::SessionLocked => {}
            // 锁屏期间合成器自己接管了焦点,解锁后要把缓存的焦点清掉,
            // 让下一个 manage 序列重新发送一次 focus_window。
            Event::SessionUnlocked => {
                state
                    .wm
                    .seats
                    .values_mut()
                    .for_each(|seat| seat.focused = None);
            }
            // 会话正常结束:不再 process::exit,交给主循环正常返回 Ok。
            Event::Finished => state.finished = true,
            // 已有别的 WM 在运行:记录错误,由 main 打印并决定退出码。
            Event::Unavailable => record_error(&mut state.err, Error::AlreadyRunning),
        }
    }

    wayland_client::event_created_child!(AppData, RiverWindowManagerV1, [
        river::river_window_manager_v1::EVT_WINDOW_OPCODE => (RiverWindowV1, ()),
        river::river_window_manager_v1::EVT_OUTPUT_OPCODE => (RiverOutputV1, ()),
        river::river_window_manager_v1::EVT_SEAT_OPCODE => (RiverSeatV1, ())
    ]);
}

wayland_client::delegate_noop!(AppData: ignore RiverXkbBindingsV1);
wayland_client::delegate_noop!(AppData: ignore RiverNodeV1);

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    const WM_VERSION: u32 = 4;
    const XKB_VERSION: u32 = 1;

    let conn = Connection::connect_to_env()?;
    let (globals, mut event_queue) = registry_queue_init::<AppData>(&conn)?;

    let river_wm = globals.bind::<RiverWindowManagerV1, _, _>(
        &event_queue.handle(),
        WM_VERSION..=WM_VERSION,
        (),
    )?;
    let river_xkb = globals.bind::<RiverXkbBindingsV1, _, _>(
        &event_queue.handle(),
        XKB_VERSION..=XKB_VERSION,
        (),
    )?;

    let mut app_data = AppData {
        river_wm,
        river_xkb,
        wm: WindowManager::default(),
        err: None,
        finished: false,
    };

    loop {
        event_queue.blocking_dispatch(&mut app_data)?;

        // 回调里没有返回值可以带错误,统一在这里检查并交给 main。
        if let Some(err) = app_data.err.take() {
            return Err(err.into());
        }
        if app_data.finished {
            return Ok(());
        }
    }
}
