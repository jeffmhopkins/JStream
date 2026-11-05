//! The modal dialog frame: a dialog centred over a dim backdrop. A press on the
//! backdrop or Escape cancels it, and Enter presses its primary button (one marked
//! `data-primary`) unless the key was meant for a text area, a button or a select.
//! A press on the dialog's own header drags it, and that place is remembered for the
//! page.
//!
//! Docs: docs/UI-FEATURES.md#dialogs

use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use leptos::web_sys::wasm_bindgen::JsCast;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Whether Enter on an element of this tag presses the dialog's primary button. A text
/// area takes Enter as a new line, and a button or a select acts on it themselves.
fn enter_submits(tag: &str) -> bool {
    !matches!(
        tag.to_ascii_lowercase().as_str(),
        "textarea" | "button" | "select"
    )
}

/// How far a drag may move one edge.
///
/// `start` is where that edge sits before `delta` is applied, `size` is the box's
/// extent and `viewport` the window's. The box stays inside `[0, viewport]`. When it
/// is larger than the window it stays covering the window instead: the edge stays at
/// or before 0 and the far side at or past `viewport`. A non-finite measurement moves
/// nothing, so a bad rect can't send the dialog to `NaN`.
fn clamp_axis(start: f64, size: f64, viewport: f64, delta: f64) -> f64 {
    if !start.is_finite() || !size.is_finite() || !viewport.is_finite() || !delta.is_finite() {
        return 0.0;
    }
    let pos = start + delta;
    let (lo, hi) = if size > viewport {
        (viewport - size, 0.0)
    } else {
        (0.0, viewport - size)
    };
    pos.clamp(lo, hi) - start
}

/// The vertical clamp. Same as [`clamp_axis`], except the top never goes above the
/// window: covering a too-tall window would pull the header off the top, and then it
/// could not be dragged back.
fn clamp_top(start: f64, size: f64, viewport: f64, delta: f64) -> f64 {
    let delta = clamp_axis(start, size, viewport, delta);
    let top = start + delta;
    if top < 0.0 { -start } else { delta }
}

/// Two header presses this close, in time and in pixels, put the dialog back in the
/// centre. Farther apart, the second press is a new drag.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
/// A press that travels no farther than this is still a click, not a drag.
const CLICK_PX: f64 = 4.0;

/// Whether a second press at `at`, `elapsed` after one at `previous`, is a double-click.
fn is_double_click(previous: Option<(f64, f64)>, at: (f64, f64), elapsed: Duration) -> bool {
    let Some((x, y)) = previous else {
        return false;
    };
    elapsed <= DOUBLE_CLICK && (at.0 - x).hypot(at.1 - y) <= CLICK_PX
}

// Where each dialog was left, by its accessible name, for this page. The engine's UI
// settings (ADR-0084) are the layout that follows the person; a dialog's place does
// not, and a reload forgets it.
thread_local! {
    static PLACED: RefCell<HashMap<&'static str, (f64, f64)>> = RefCell::new(HashMap::new());
}

/// The offset remembered for `label`, or the centred place.
fn remembered(label: &'static str) -> (f64, f64) {
    PLACED.with(|placed| placed.borrow().get(label).copied().unwrap_or((0.0, 0.0)))
}

/// Remembers `at` as where `label`'s dialog was left.
fn remember(label: &'static str, at: (f64, f64)) {
    PLACED.with(|placed| {
        placed.borrow_mut().insert(label, at);
    });
}

/// The window's inner size in CSS pixels. Zero when the page has no size yet.
fn viewport() -> (f64, f64) {
    let size = |value: Result<leptos::wasm_bindgen::JsValue, _>| {
        value.ok().and_then(|value| value.as_f64()).unwrap_or(0.0)
    };
    (size(window().inner_width()), size(window().inner_height()))
}

/// A drag's start: the pointer, the offset then, and the box already on screen.
#[derive(Clone, Copy)]
struct Drag {
    id: i32,
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
    left: f64,
    top: f64,
    width: f64,
    height: f64,
    vw: f64,
    vh: f64,
}

impl Drag {
    /// The offset after the pointer has moved to `at`.
    fn at(self, at: (f64, f64)) -> (f64, f64) {
        (
            self.dx + clamp_axis(self.left, self.width, self.vw, at.0 - self.x),
            self.dy + clamp_top(self.top, self.height, self.vh, at.1 - self.y),
        )
    }
}

/// Whether `target` is inside `dialog`'s own header and not on a control there. A
/// dialog opened over another (Add filter over the Filters window) has its own header,
/// and the ×, a field or a switch keeps its own press.
fn header_drag(dialog: &leptos::web_sys::Element, target: &leptos::web_sys::Element) -> bool {
    let Ok(Some(header)) = target.closest(".modal-header") else {
        return false;
    };
    if header.closest(".modal").ok().flatten().as_ref() != Some(dialog) {
        return false;
    }
    matches!(
        target.closest("button, input, select, textarea, a, [role=switch]"),
        Ok(None)
    )
}

/// The point a pointer event names.
fn at_pointer(event: &ev::PointerEvent) -> (f64, f64) {
    (f64::from(event.client_x()), f64::from(event.client_y()))
}

/// A modal dialog.
#[component]
pub fn Modal(
    /// The dialog's accessible name. Also the key its place is remembered under.
    label: &'static str,
    /// Extra classes for the dialog box (its width).
    #[prop(optional)]
    class: &'static str,
    /// Cancels the dialog: the backdrop, Escape and the header's ×.
    cancel: impl Fn() + Copy + 'static,
    /// Leaves what is under it undimmed, as the Filters window does, so the canvas
    /// shows the change.
    #[prop(optional)]
    undimmed: bool,
    children: Children,
) -> impl IntoView {
    let dialog = NodeRef::<html::Div>::new();
    let offset = RwSignal::new(remembered(label));
    // The drag in progress, and the last header click that did not itself drag, so a
    // second press can put the dialog back in the centre.
    let drag = StoredValue::new(None::<Drag>);
    let click = StoredValue::new(None::<(Instant, f64, f64)>);
    let place = move |next: (f64, f64)| {
        if offset.get_untracked() != next {
            offset.set(next);
            remember(label, next);
        }
    };
    // Focus the dialog's first field, so Enter and Escape reach it at once, and pull a
    // remembered place back on screen in case the window shrank.
    request_animation_frame(move || {
        if let Some(dialog) = dialog.get_untracked() {
            let first = dialog
                .query_selector("input:not([type=radio]), select, textarea")
                .ok()
                .flatten()
                .and_then(|e| e.dyn_into::<leptos::web_sys::HtmlElement>().ok());
            let _ = match first {
                Some(field) => field.focus(),
                None => dialog.focus(),
            };
            if drag.get_value().is_none() {
                let rect = dialog.get_bounding_client_rect();
                let (vw, vh) = viewport();
                let (dx, dy) = offset.get_untracked();
                place((
                    dx + clamp_axis(rect.left(), rect.width(), vw, 0.0),
                    dy + clamp_top(rect.top(), rect.height(), vh, 0.0),
                ));
            }
        }
    });
    let follow = move |event: &ev::PointerEvent| {
        let Some(state) = drag.get_value() else {
            return;
        };
        if event.pointer_id() != state.id {
            return;
        }
        place(state.at(at_pointer(event)));
    };
    let end_drag = move |event: &ev::PointerEvent, as_click: bool| {
        let Some(state) = drag.get_value() else {
            return;
        };
        if event.pointer_id() != state.id {
            return;
        }
        let at = at_pointer(event);
        drag.set_value(None);
        // A press that barely moved can be the first of a double-click. A drag, or a
        // cancelled press, cannot.
        if as_click && (at.0 - state.x).hypot(at.1 - state.y) <= CLICK_PX {
            click.set_value(Some((Instant::now(), state.x, state.y)));
        } else {
            click.set_value(None);
        }
    };
    let on_key = move |event: ev::KeyboardEvent| match event.key().as_str() {
        "Escape" => {
            event.prevent_default();
            event.stop_propagation();
            cancel();
        }
        "Enter" => {
            let tag = event
                .target()
                .and_then(|t| t.dyn_into::<leptos::web_sys::Element>().ok())
                .map(|e| e.tag_name())
                .unwrap_or_default();
            if !enter_submits(&tag) {
                return;
            }
            let primary = dialog
                .get_untracked()
                .and_then(|d| d.query_selector("[data-primary]").ok().flatten())
                .and_then(|e| e.dyn_into::<leptos::web_sys::HtmlElement>().ok());
            if let Some(primary) = primary {
                event.prevent_default();
                // A field commits on `change`, which the prevented Enter no longer
                // fires: blurring it fires it now, so the primary button reads the
                // typed value.
                if let Some(field) = event
                    .target()
                    .and_then(|t| t.dyn_into::<leptos::web_sys::HtmlElement>().ok())
                {
                    let _ = field.blur();
                    primary.click();
                    // Back to the field, should the dialog stay open (a refused OK).
                    let _ = field.focus();
                } else {
                    primary.click();
                }
            }
        }
        _ => {}
    };
    let on_down = move |event: ev::PointerEvent| {
        // The backdrop cancels on any press that reaches it. A press on the box never
        // does, including a right-click and a press on a control.
        event.stop_propagation();
        if event.button() != 0 {
            return;
        }
        let Some(dialog) = dialog.get_untracked() else {
            return;
        };
        let Some(target) = event
            .target()
            .and_then(|t| t.dyn_into::<leptos::web_sys::Element>().ok())
        else {
            return;
        };
        if !header_drag(&dialog, &target) {
            return;
        }
        // Keep the title from being selected, and keep the moves coming once the
        // pointer leaves the header.
        event.prevent_default();
        let at = at_pointer(&event);
        if let Some((when, x, y)) = click.get_value()
            && is_double_click(Some((x, y)), at, when.elapsed())
        {
            click.set_value(None);
            place((0.0, 0.0));
            return;
        }
        let _ = dialog.set_pointer_capture(event.pointer_id());
        let rect = dialog.get_bounding_client_rect();
        let (vw, vh) = viewport();
        let (dx, dy) = offset.get_untracked();
        drag.set_value(Some(Drag {
            id: event.pointer_id(),
            x: at.0,
            y: at.1,
            dx,
            dy,
            left: rect.left(),
            top: rect.top(),
            width: rect.width(),
            height: rect.height(),
            vw,
            vh,
        }));
    };
    view! {
        <div class="modal-backdrop" class:undimmed=undimmed on:pointerdown=move |_| cancel()>
            <div
                class=format!("modal {class}")
                role="dialog"
                aria-modal="true"
                aria-label=label
                tabindex="-1"
                node_ref=dialog
                style:transform=move || {
                    let (x, y) = offset.get();
                    (x != 0.0 || y != 0.0).then(|| format!("translate({x}px, {y}px)"))
                }
                on:keydown=on_key
                on:pointerdown=on_down
                on:pointermove=move |event: ev::PointerEvent| {
                    follow(&event);
                    // A release the dialog never saw (outside the window, before
                    // capture) leaves no button held.
                    if event.buttons() == 0 {
                        end_drag(&event, true);
                    }
                }
                on:pointerup=move |event: ev::PointerEvent| {
                    follow(&event);
                    end_drag(&event, true);
                }
                on:pointercancel=move |event: ev::PointerEvent| end_drag(&event, false)
                on:contextmenu=|event: ev::MouseEvent| event.stop_propagation()
            >
                {children()}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::{CLICK_PX, DOUBLE_CLICK, clamp_axis, clamp_top, enter_submits, is_double_click};
    use std::time::Duration;

    #[test]
    fn enter_presses_the_primary_button_from_fields_only() {
        assert!(enter_submits("INPUT"));
        assert!(enter_submits("div"));
        assert!(!enter_submits("TEXTAREA"));
        assert!(!enter_submits("BUTTON"));
        assert!(!enter_submits("select"));
    }

    #[test]
    fn a_drag_keeps_the_box_inside_the_window() {
        assert_eq!(clamp_axis(100.0, 200.0, 800.0, 50.0), 50.0);
        // The right edge would pass the window: the box stops flush with it.
        assert_eq!(clamp_axis(100.0, 200.0, 800.0, 1000.0), 500.0);
        // The left edge would pass 0.
        assert_eq!(clamp_axis(100.0, 200.0, 800.0, -500.0), -100.0);
        // A box that already fills the window stays put.
        assert_eq!(clamp_axis(0.0, 800.0, 800.0, 40.0), 0.0);
    }

    #[test]
    fn a_box_wider_than_the_window_keeps_covering_it() {
        // Covering positions for a 1000px box in an 800px window are -200 to 0.
        assert_eq!(clamp_axis(-100.0, 1000.0, 800.0, 0.0), 0.0);
        assert_eq!(clamp_axis(-100.0, 1000.0, 800.0, 250.0), 100.0);
        assert_eq!(clamp_axis(-100.0, 1000.0, 800.0, -250.0), -100.0);
    }

    #[test]
    fn the_top_never_goes_above_the_window() {
        assert_eq!(clamp_top(40.0, 200.0, 800.0, -100.0), -40.0);
        assert_eq!(clamp_top(40.0, 200.0, 800.0, 1000.0), 560.0);
        // Taller than the window: covering would pull the header above 0.
        assert_eq!(clamp_top(-80.0, 1000.0, 800.0, -40.0), 80.0);
        assert_eq!(clamp_top(20.0, 1000.0, 800.0, 100.0), -20.0);
        assert_eq!(clamp_top(0.0, 1000.0, 800.0, -30.0), 0.0);
    }

    #[test]
    fn a_bad_measurement_does_not_move_the_box() {
        assert_eq!(clamp_axis(10.0, f64::NAN, 800.0, 5.0), 0.0);
        assert_eq!(clamp_top(10.0, 100.0, f64::INFINITY, 5.0), 0.0);
    }

    #[test]
    fn a_second_press_close_and_soon_is_a_double_click() {
        let at = (10.0, 20.0);
        assert!(is_double_click(
            Some(at),
            (12.0, 21.0),
            Duration::from_millis(200)
        ));
        assert!(is_double_click(Some(at), at, DOUBLE_CLICK));
        assert!(!is_double_click(None, at, Duration::from_millis(10)));
        assert!(!is_double_click(
            Some((at.0 + CLICK_PX + 1.0, at.1)),
            at,
            Duration::from_millis(10)
        ));
        assert!(!is_double_click(
            Some(at),
            at,
            DOUBLE_CLICK + Duration::from_millis(1)
        ));
    }
}
