//! The Sequence panel's zoom bar, along the bottom of its lanes, as the Show panel's Edit
//! view has one: a bar for the whole sequence with a thumb over the part in view. A handle
//! at either end of the thumb zooms, its middle scrolls, a press on the bar outside it
//! brings the view there and a double-click shows the whole sequence. From the keyboard,
//! the arrows scroll and + and − zoom. Over the lanes, Ctrl+wheel zooms about the pointer
//! and Shift+wheel (or a sideways wheel) scrolls.
//!
//! The view is the Show panel's [`Viewport`], over a sequence counted as [`SEQ_LEN`] long
//! whatever its length, so it zooms in to a fiftieth of any sequence. The ruler, the lanes,
//! the Events lane and the play head draw the whole sequence as before, in a frame
//! [`frame`] widens and slides left inside their column; a press finds its time from that
//! frame, so nothing else changes with the zoom.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::panels::show::edit::geom::{self, Grip, ZoomDrag};
use crate::panels::show::time::{MIN_SPAN_MS, Viewport};
use leptos::prelude::*;
use leptos::wasm_bindgen::JsCast;

/// The length the view counts any sequence as: [`MIN_SPAN_MS`] of it is the most the
/// view zooms in, a fiftieth.
pub const SEQ_LEN: f64 = MIN_SPAN_MS * 50.0;

/// The whole sequence in view.
pub fn fit() -> Viewport {
    Viewport::fit(SEQ_LEN as u32)
}

/// The `width` and `margin-left` styles for a lane's frame, in a column whose drawing area
/// is inset `inset` px each side: the whole sequence, as wide as the view needs, slid
/// left to the part in view.
pub fn frame(view: Viewport, inset: f64) -> (String, String) {
    let k = view.len_ms / view.span_ms.max(1.0);
    let from = view.start_ms / view.len_ms.max(1.0);
    let edge = 2.0 * inset;
    (
        format!("calc((100% - {edge}px) * {k:.6})"),
        format!("calc({inset}px - (100% - {edge}px) * {:.6})", k * from),
    )
}

/// The ruler's step, in ms, for a sequence `duration_ms` long with `view` in view: a round
/// step with at most about eight labels across the part in view.
pub fn tick_step(duration_ms: u32, view: Viewport) -> u32 {
    let visible = f64::from(duration_ms) * view.span_ms / view.len_ms.max(1.0);
    [
        100, 200, 500, 1000, 2000, 5000, 10_000, 30_000, 60_000, 300_000, 600_000,
    ]
    .into_iter()
    .find(|&s| visible / f64::from(s) <= 8.0)
    .unwrap_or(1_800_000)
}

/// What a wheel over the lanes does to `zoom`, `x` px across a time column `width` px
/// wide. Returns whether it took the wheel.
pub fn wheel(zoom: RwSignal<Viewport>, e: &leptos::ev::WheelEvent, x: f64, width: f64) -> bool {
    let next = geom::wheeled(
        zoom.get_untracked(),
        (e.delta_x(), e.delta_y()),
        e.ctrl_key() || e.meta_key(),
        e.shift_key(),
        x,
        width,
    );
    if let Some(next) = next {
        zoom.set(next);
        true
    } else {
        false
    }
}

/// The zoom bar, with "Whole sequence · 0.8 s" at its left and the play head on it.
#[component]
pub fn SeqZoomBar(
    /// The part of the sequence in view.
    zoom: RwSignal<Viewport>,
    /// The sequence's length.
    duration_ms: u32,
    /// Where the play head is, while the sequence plays.
    position: Signal<Option<u32>>,
) -> impl IntoView {
    let drag = StoredValue::new(None::<ZoomDrag>);
    let along = |e: &leptos::ev::PointerEvent| {
        let el = e
            .current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())?;
        let rect = el.get_bounding_client_rect();
        Some((f64::from(e.client_x()) - rect.left(), rect.width(), el))
    };
    let down = move |e: leptos::ev::PointerEvent| {
        if e.button() != 0 {
            return;
        }
        let Some((x, width, el)) = along(&e) else {
            return;
        };
        e.prevent_default();
        el.set_pointer_capture(e.pointer_id()).ok();
        let view = zoom.get_untracked();
        let grip = geom::grip_at(view, x, width);
        let from = if grip == Grip::Outside {
            let centred = geom::centred_at(view, x, width);
            zoom.set(centred);
            centred
        } else {
            view
        };
        drag.set_value(Some(ZoomDrag {
            grip,
            from_x: x,
            from,
        }));
    };
    let moving = move |e: leptos::ev::PointerEvent| {
        let Some(d) = drag.get_value() else {
            return;
        };
        if let Some((x, width, _)) = along(&e) {
            zoom.set(d.at(x, width));
        }
    };
    let up = move |_: leptos::ev::PointerEvent| drag.set_value(None);
    let key = move |e: leptos::ev::KeyboardEvent| {
        let v = zoom.get_untracked();
        let next = match e.key().as_str() {
            "ArrowLeft" => v.scrolled(-v.span_ms / 10.0),
            "ArrowRight" => v.scrolled(v.span_ms / 10.0),
            "+" | "=" => v.zoom_at(0.8, 0.5, 1.0),
            "-" | "_" => v.zoom_at(1.25, 0.5, 1.0),
            "Home" => v.scrolled(-v.len_ms),
            "End" => v.scrolled(v.len_ms),
            _ => return,
        };
        e.prevent_default();
        zoom.set(next);
    };
    let duration = duration_ms.max(1);
    let head = move || {
        position.get().map(|ms| {
            let left = (f64::from(ms) / f64::from(duration) * 100.0).clamp(0.0, 100.0);
            view! { <i class="show-zoom-head" style:left=format!("{left:.3}%")></i> }
        })
    };
    let thumb = move || zoom.with(|v| v.thumb());
    view! {
        <div class="seq-zoom-row">
            <span class="seq-zoom-label">{format!("Whole sequence · {}", crate::panels::sequence::seconds(duration_ms))}</span>
            <div class="show-zoom" tabindex="0" role="scrollbar" aria-orientation="horizontal"
                aria-label="The part of the sequence in view"
                aria-valuemin="0" aria-valuemax="100"
                aria-valuenow=move || format!("{:.0}", thumb().0 * 100.0)
                title="Drag an end to zoom, the middle to scroll · double-click to see the whole sequence"
                on:pointerdown=down on:pointermove=moving on:pointerup=up on:pointercancel=up
                on:dblclick=move |_| zoom.set(fit()) on:keydown=key>
                {head}
                <div class="show-thumb"
                    style:left=move || format!("{:.3}%", thumb().0 * 100.0)
                    style:width=move || format!("{:.3}%", (thumb().1 - thumb().0) * 100.0)>
                    <i class="show-grip show-grip-l"></i>
                    <i class="show-grip show-grip-r"></i>
                </div>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_sequence_fills_its_column() {
        let (width, left) = frame(fit(), 8.0);
        assert_eq!(width, "calc((100% - 16px) * 1.000000)");
        assert_eq!(left, "calc(8px - (100% - 16px) * 0.000000)");
    }

    #[test]
    fn a_zoomed_view_widens_the_frame_and_slides_it_left() {
        // The second quarter of the sequence in view: four times as wide, a quarter of
        // that slid out to the left.
        let view = Viewport {
            start_ms: SEQ_LEN / 4.0,
            span_ms: SEQ_LEN / 4.0,
            len_ms: SEQ_LEN,
        };
        let (width, left) = frame(view, 0.0);
        assert_eq!(width, "calc((100% - 0px) * 4.000000)");
        assert_eq!(left, "calc(0px - (100% - 0px) * 1.000000)");
    }

    #[test]
    fn the_view_zooms_in_to_a_fiftieth() {
        let v = fit().zoom_at(0.001, 0.0, 1000.0);
        assert!((v.span_ms - SEQ_LEN / 50.0).abs() < 1e-6);
    }

    #[test]
    fn the_ruler_steps_finer_as_the_view_zooms_in() {
        assert_eq!(tick_step(800, fit()), 100);
        assert_eq!(tick_step(30_000, fit()), 5000);
        let tenth = Viewport {
            start_ms: 0.0,
            span_ms: SEQ_LEN / 10.0,
            len_ms: SEQ_LEN,
        };
        assert_eq!(tick_step(30_000, tenth), 500);
    }
}
