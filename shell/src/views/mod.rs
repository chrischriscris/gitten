//! Each view is a self-contained entity that fills whatever box it is handed.
//! None of them assume they own the window or the keymap — that is what makes
//! assembling the final multi-pane layout an assembly job rather than a rewrite.

use gpui::{point, px, Bounds, Pixels, Point, Size, UniformListScrollHandle};
use gpui_component::scroll::ScrollbarHandle;
use std::cell::Cell;
use std::rc::Rc;

/// State spanning a strict list request and the prepaint that consumes it.
///
/// Wheel events can arrive after a reflow parks its row but before the next
/// prepaint. Their exact pixels wait here; converting each event to a row would
/// drop a trackpad's small deltas. A scrollbar drag cancels both through
/// [`DeferredScrollbar`], because the user's newer position wins.
#[derive(Clone, Default)]
struct PendingScroll(Rc<PendingScrollState>);

#[derive(Default)]
struct PendingScrollState {
    awaiting: Cell<bool>,
    wheel: Cell<f32>,
}

impl PendingScroll {
    fn is_awaiting(&self) -> bool {
        self.0.awaiting.get()
    }

    fn begin(&self) {
        self.0.wheel.set(0.0);
        self.0.awaiting.set(true);
    }

    fn wheel(&self, dy: f32) -> f32 {
        let total = self.0.wheel.get() + dy;
        self.0.wheel.set(total);
        total
    }

    fn cancel(&self) {
        self.0.wheel.set(0.0);
        self.0.awaiting.set(false);
    }
}

#[derive(Clone, Copy)]
struct AcceptedScroll {
    y: f32,
    wheeled: bool,
}

/// Accepts the offset written when `uniform_list` consumes a deferred request.
///
/// The row callback also runs for measurement, before prepaint takes the
/// request. Only an awaiting marker with no request left means the strict offset
/// is real. Any wheel pixels accumulated meanwhile are applied against the now
/// current bound and reported so the caller can update its viewport model.
fn accept_deferred_scroll(
    scroll: &UniformListScrollHandle,
    pending: &PendingScroll,
    synced: &Cell<f32>,
) -> Option<AcceptedScroll> {
    if !pending.0.awaiting.get() {
        return None;
    }
    let state = scroll.0.borrow();
    if state.deferred_scroll_to_item.is_some() {
        return None;
    }
    let offset = state.base_handle.offset();
    let wheel = pending.0.wheel.replace(0.0);
    let wheeled = wheel != 0.0;
    // GPUI has already clamped the strict offset. Only a wheel delta needs a
    // new clamp; touching a consumed offset with no wheel would make a headless
    // handle (whose test bound is zero) erase the position being accepted.
    let y = match wheeled {
        true => {
            (f32::from(offset.y) + wheel).clamp(-f32::from(state.base_handle.max_offset().y), 0.0)
        }
        false => f32::from(offset.y),
    };
    if y != f32::from(offset.y) {
        state.base_handle.set_offset(point(offset.x, px(y)));
    }
    synced.set(y);
    pending.0.awaiting.set(false);
    Some(AcceptedScroll { y, wheeled })
}

/// The vertical bar's handle. Unlike gpui-component's blanket implementation,
/// a thumb write through this one cancels a not-yet-consumed strict request.
#[derive(Clone)]
struct DeferredScrollbar {
    scroll: UniformListScrollHandle,
    pending: PendingScroll,
}

impl DeferredScrollbar {
    fn new(scroll: &UniformListScrollHandle, pending: &PendingScroll) -> Self {
        Self {
            scroll: scroll.clone(),
            pending: pending.clone(),
        }
    }
}

impl ScrollbarHandle for DeferredScrollbar {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.scroll.0.borrow().base_handle.bounds()
    }

    fn offset(&self) -> Point<Pixels> {
        self.scroll.0.borrow().base_handle.offset()
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        self.pending.cancel();
        let mut state = self.scroll.0.borrow_mut();
        state.deferred_scroll_to_item = None;
        state.base_handle.set_offset(offset);
    }

    fn content_size(&self) -> Size<Pixels> {
        let state = self.scroll.0.borrow();
        let base = &state.base_handle;
        (base.max_offset() + base.bounds().size.into()).into()
    }
}

pub mod branches;
pub mod commits;
pub mod diff;
pub mod files;
pub mod markdown;
pub mod split;
pub mod stashes;
pub mod status;
