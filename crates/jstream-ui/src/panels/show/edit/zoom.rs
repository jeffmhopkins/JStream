//! The Edit view's zoom bar, along its bottom: one bar for the whole show with a small
//! overview on it (program in its colours, the bookmarks, the play head), and a thumb over
//! the part in view. A handle at either end of the thumb zooms, its middle scrolls, a
//! press on the bar outside it brings the view there and a double-click fits the show.
//! From the keyboard, the arrows scroll and + and − zoom.
//!
//! Docs: crates/jstream-ui/README.md#design

use super::super::ShowPanel;
use super::super::model::ShowView;
use super::super::time::{Viewport, clock};
use super::geom::{self, Grip, ZoomDrag};
use leptos::prelude::*;
use leptos::wasm_bindgen::JsCast;

/// The zoom bar, with "Whole show · 11:00" at its left.
#[component]
pub fn ZoomBar(
    /// The open show.
    show: Memo<Option<ShowView>>,
) -> impl IntoView {
    let panel = expect_context::<ShowPanel>();
    let viewport = panel.viewport;
    let drag = StoredValue::new(None::<ZoomDrag>);
    // Where a pointer is along the bar, and the bar's width, in px.
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
        let view = viewport.get_untracked();
        let grip = geom::grip_at(view, x, width);
        let from = if grip == Grip::Outside {
            let centred = geom::centred_at(view, x, width);
            viewport.set(centred);
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
            viewport.set(d.at(x, width));
        }
    };
    let up = move |_: leptos::ev::PointerEvent| drag.set_value(None);
    let fit = move |_| viewport.update(|v| *v = Viewport::fit(v.len_ms as u32));
    let key = move |e: leptos::ev::KeyboardEvent| {
        let v = viewport.get_untracked();
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
        viewport.set(next);
    };
    let overview = move || {
        let len = viewport.with(|v| v.len_ms);
        show.with(|s| {
            let Some(s) = s else {
                return Vec::new();
            };
            let segments = geom::overview(s, len).into_iter().map(|g| {
                view! {
                    <i class=format!("show-mini show-clip-c{}", g.colour)
                        style:left=format!("{:.3}%", g.left * 100.0)
                        style:width=format!("{:.3}%", g.width * 100.0)></i>
                }
                .into_any()
            });
            let marks = s.bookmarks.iter().map(|b| {
                let left = f64::from(b.at_ms) / len.max(1.0) * 100.0;
                view! { <i class="show-zoom-bk" style:left=format!("{left:.3}%")></i> }.into_any()
            });
            segments.chain(marks).collect::<Vec<_>>()
        })
    };
    let head = move || {
        panel.is_running().then(|| {
            let len = viewport.with(|v| v.len_ms).max(1.0);
            let left = (f64::from(panel.head()) / len * 100.0).clamp(0.0, 100.0);
            view! { <i class="show-zoom-head" style:left=format!("{left:.3}%")></i> }
        })
    };
    let thumb = move || viewport.with(|v| v.thumb());
    let label = move || {
        let len = show.with(|s| s.as_ref().map_or(0, ShowView::length_ms));
        format!("Whole show · {}", clock(len))
    };
    view! {
        <div class="show-zoom-row">
            <span class="show-zoom-label">{label}</span>
            <div class="show-zoom" tabindex="0" role="scrollbar" aria-orientation="horizontal"
                aria-label="The part of the show in view"
                aria-valuemin="0" aria-valuemax="100"
                aria-valuenow=move || format!("{:.0}", thumb().0 * 100.0)
                title="Drag an end to zoom, the middle to scroll · double-click to fit the show"
                on:pointerdown=down on:pointermove=moving on:pointerup=up on:pointercancel=up
                on:dblclick=fit on:keydown=key>
                {overview}
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
