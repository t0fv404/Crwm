use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

use crate::river::river_node_v1::RiverNodeV1;
use crate::river::river_seat_v1::RiverSeatV1;
use crate::river::river_window_v1::{Edges, RiverWindowV1};
use crate::{AppData, Error, record_error};

/// 一个被管理的窗口。
///
/// 字段是 `pub(crate)` 的:crate 根模块里的 `WindowManager` 需要直接读写它们
/// (位置、尺寸、待处理的指针请求),这样拆分文件之后不必再包一层访问方法。
#[derive(Debug)]
pub(crate) struct Window {
    pub(crate) proxy: RiverWindowV1,
    pub(crate) node: RiverNodeV1,
    pub(crate) new: bool,
    pub(crate) closed: bool,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) pointer_move_requested: Option<RiverSeatV1>,
    pub(crate) pointer_resize_requested: Option<RiverSeatV1>,
    pub(crate) pointer_resize_requested_edges: Edges,
}

impl Window {
    pub(crate) fn new(proxy: RiverWindowV1, qh: &QueueHandle<AppData>) -> Self {
        let node = proxy.get_node(qh, ());
        Window {
            proxy,
            node,
            new: true,
            closed: false,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            pointer_move_requested: None,
            pointer_resize_requested: None,
            pointer_resize_requested_edges: Edges::None,
        }
    }

    pub(crate) fn set_position(&mut self, x: i32, y: i32) {
        self.node.set_position(x, y);
        self.x = x;
        self.y = y;
    }
}

impl Dispatch<RiverWindowV1, ()> for AppData {
    fn event(
        state: &mut Self,
        proxy: &RiverWindowV1,
        event: <RiverWindowV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use crate::river::river_window_v1::Event;

        // 窗口已经在本轮 manage 里被移除时直接跳过(事件与销毁的竞态),不 panic。
        let Some(window) = state
            .wm
            .windows
            .iter_mut()
            .find(|window| &window.proxy == proxy)
        else {
            return;
        };

        // 注意:`window` 一直可变借用到 match 结束,所以这里不能在分支里调用
        // `state.fail(..)`(那需要借用整个 state),先记标志,出了 match 再记录。
        let mut invalid_edges = false;
        match event {
            Event::Closed => window.closed = true,
            Event::Dimensions { width, height } => (window.width, window.height) = (width, height),
            Event::PointerMoveRequested { seat } => window.pointer_move_requested = Some(seat),
            Event::PointerResizeRequested { seat, edges } => match edges.into_result() {
                Ok(edges) => {
                    window.pointer_resize_requested = Some(seat);
                    window.pointer_resize_requested_edges = edges;
                }
                // 协议被违反:不 panic,交给 main 处理
                Err(_) => invalid_edges = true,
            },
            _ => {}
        }

        if invalid_edges {
            record_error(&mut state.err, Error::InvalidEdges);
        }
    }
}
