//! Each view is a self-contained entity that fills whatever box it is handed.
//! None of them assume they own the window or the keymap — that is what makes
//! assembling the final multi-pane layout an assembly job rather than a rewrite.

use gpui::{point, px, Bounds, Pixels, Point, Size, UniformListScrollHandle};
use gpui_component::scroll::{Scrollbar, ScrollbarAxis, ScrollbarHandle};
use std::cell::Cell;
use std::rc::Rc;

/// Gitten's scrollbar geometry: one quiet, square bar against the container's
/// edge, rather than gpui-component's narrow thumb inset inside a wider track.
///
/// Keep this beside the shared handle adapter so every built-in pane gets the
/// same furniture. The library still owns painting, hit testing and dragging;
/// this only supplies the presentation it deliberately exposes as data.
///
/// The scrollbar's track — and thumb, the two are one square bar — in px.
///
/// The one number right-edge furniture in a scrollable pane must agree with:
/// the track overlays the panes' right edge, so every section count, drift
/// count and clock that ends a row reserves exactly this much, by taking
/// this constant rather than spelling its own pixels beside it.
pub(crate) const SCROLLBAR_TRACK_W: f32 = 8.0;

fn scrollbar<H: ScrollbarHandle + Clone>(handle: &H, axis: ScrollbarAxis) -> Scrollbar {
    Scrollbar::new(handle).axis(axis).styles(|s| {
        s.track(|s| s.width(px(SCROLLBAR_TRACK_W)))
            .thumb(|s| s.width(px(SCROLLBAR_TRACK_W)).inset(px(0.)).radius(px(0.)))
    })
}

fn vertical_scrollbar<H: ScrollbarHandle + Clone>(handle: &H) -> Scrollbar {
    scrollbar(handle, ScrollbarAxis::Vertical)
}

fn horizontal_scrollbar<H: ScrollbarHandle + Clone>(handle: &H) -> Scrollbar {
    scrollbar(handle, ScrollbarAxis::Horizontal)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::ScrollStrategy;

    /// A handle with a strict request parked on it, as `scroll_to` leaves one.
    fn parked(row: usize) -> (UniformListScrollHandle, PendingScroll, Cell<f32>) {
        let scroll = UniformListScrollHandle::new();
        let pending = PendingScroll::default();
        pending.begin();
        scroll.scroll_to_item_strict(row, ScrollStrategy::Top);
        (scroll, pending, Cell::new(0.0))
    }

    /// What prepaint does when it consumes the request: clears it and writes
    /// the offset it settled on.
    fn prepaint(scroll: &UniformListScrollHandle, y: f32) {
        let mut state = scroll.0.borrow_mut();
        state.deferred_scroll_to_item = None;
        state.base_handle.set_offset(point(px(0.), px(y)));
    }

    #[test]
    fn an_offset_is_accepted_only_once_the_request_is_gone() {
        // The row callback runs for measurement *before* prepaint takes the
        // request, and the offset it would read then is the one from before the
        // restore. Accepting it is how a restored position silently becomes
        // row zero.
        let (scroll, pending, synced) = parked(40);
        assert!(accept_deferred_scroll(&scroll, &pending, &synced).is_none());
        assert!(pending.is_awaiting(), "still waiting on prepaint");
        assert_eq!(synced.get(), 0.0);

        prepaint(&scroll, -880.0);
        let accepted =
            accept_deferred_scroll(&scroll, &pending, &synced).expect("prepaint's offset");
        assert!(!accepted.wheeled);
        assert_eq!(accepted.y, -880.0);
        assert_eq!(synced.get(), -880.0);
        assert!(!pending.is_awaiting(), "and it stops waiting");

        // Once, and not again: a second call has nothing left to accept.
        assert!(accept_deferred_scroll(&scroll, &pending, &synced).is_none());
    }

    #[test]
    fn nothing_awaiting_accepts_nothing() {
        let scroll = UniformListScrollHandle::new();
        let pending = PendingScroll::default();
        let synced = Cell::new(0.0);
        prepaint(&scroll, -100.0);
        assert!(accept_deferred_scroll(&scroll, &pending, &synced).is_none());
        assert_eq!(synced.get(), 0.0, "an unasked-for offset was adopted");
    }

    #[test]
    fn wheel_pixels_that_arrived_while_waiting_are_applied_on_top() {
        // A trackpad delivers fifty small deltas a second and a reflow parks a
        // row between two of them. Converting each to a row would round every
        // one of them to nothing, so they accumulate as pixels and land against
        // the bound that exists once the restore has settled.
        let (scroll, pending, synced) = parked(40);
        assert_eq!(pending.wheel(-6.0), -6.0);
        assert_eq!(
            pending.wheel(-4.0),
            -10.0,
            "they add up rather than replace"
        );

        prepaint(&scroll, -880.0);
        let accepted = accept_deferred_scroll(&scroll, &pending, &synced).expect("accepted");
        assert!(
            accepted.wheeled,
            "the caller has to know to move its cursor"
        );
        // Clamped against the handle's bound, which is zero on a headless one —
        // so the whole gesture lands at the top rather than off the end.
        assert_eq!(accepted.y, 0.0);
        assert_eq!(synced.get(), 0.0);
        assert_eq!(
            pending.0.wheel.get(),
            0.0,
            "spent, not re-applied next frame"
        );
    }

    #[test]
    fn a_consumed_offset_with_no_wheel_is_left_exactly_as_prepaint_wrote_it() {
        // The clamp is only for wheel pixels. GPUI has already bounded the
        // strict offset, and re-clamping it against a headless handle — whose
        // bound is zero — erases the very position being accepted.
        let (scroll, pending, synced) = parked(40);
        prepaint(&scroll, -880.0);
        let accepted = accept_deferred_scroll(&scroll, &pending, &synced).expect("accepted");
        assert_eq!(accepted.y, -880.0);
        assert_eq!(f32::from(scroll.0.borrow().base_handle.offset().y), -880.0);
    }

    #[test]
    fn a_thumb_write_cancels_the_request_it_would_otherwise_fight() {
        // The user's newer position wins. Without this the parked request is
        // consumed a frame later and drags the list back out from under the
        // thumb that was just released.
        let (scroll, pending, synced) = parked(40);
        let bar = DeferredScrollbar::new(&scroll, &pending);
        bar.set_offset(point(px(0.), px(-220.)));

        assert!(scroll.0.borrow().deferred_scroll_to_item.is_none());
        assert!(!pending.is_awaiting());
        assert_eq!(f32::from(bar.offset().y), -220.0);
        // And nothing is left to accept, so the next callback reads the drag
        // rather than a restore that no longer applies.
        assert!(accept_deferred_scroll(&scroll, &pending, &synced).is_none());
    }

    #[test]
    fn cancelling_forgets_the_pixels_as_well_as_the_wait() {
        // Both halves: a wheel delta surviving a cancel is applied against the
        // *next* restore, which is a scroll nobody asked for.
        let pending = PendingScroll::default();
        pending.begin();
        pending.wheel(-12.0);
        pending.cancel();
        assert!(!pending.is_awaiting());
        assert_eq!(pending.wheel(0.0), 0.0);

        // And `begin` clears them too, so a second restore starts clean.
        pending.wheel(-12.0);
        pending.begin();
        assert_eq!(pending.wheel(0.0), 0.0);
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
