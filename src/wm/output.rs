use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

use crate::river::river_output_v1::RiverOutputV1;
use crate::{AppData, Error, record_error};

/// 一个逻辑输出。
///
/// `proxy` / `removed` 需要 `pub(crate)`:移除对象由根模块的 `WindowManager` 统一做。
#[derive(Debug)]
pub(crate) struct Output {
    pub(crate) proxy: RiverOutputV1,
    pub(crate) removed: bool,
}

impl Output {
    pub(crate) fn new(proxy: RiverOutputV1) -> Self {
        Self {
            proxy,
            removed: false,
        }
    }
}

impl Dispatch<RiverOutputV1, ()> for AppData {
    fn event(
        state: &mut Self,
        proxy: &RiverOutputV1,
        event: <RiverOutputV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use crate::river::river_output_v1::Event;

        // 输出对象已被销毁时事件可能迟到:记录错误并跳过,而不是 panic。
        let Some(output) = state.wm.outputs.get_mut(&proxy.id()) else {
            record_error(&mut state.err, Error::MissingOutput);
            return;
        };
        match event {
            Event::Removed => output.removed = true,
            // 下面这些状态目前用不到,保持显式列出,避免以后加分支时看漏。
            Event::WlOutput { name: _ } => {}
            Event::Position { x: _, y: _ } => {}
            Event::Dimensions {
                width: _,
                height: _,
            } => {}
        }
    }
}
