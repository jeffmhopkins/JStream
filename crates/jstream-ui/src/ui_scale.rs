//! The UI scale: how large the whole interface is drawn on this machine, from 50 % to
//! 150 %, picked in View › UI scale or with Ctrl and `=`, `-` or `0`. It is kept in this
//! browser's storage (the desktop app's webview has its own), not in the engine's UI
//! settings, so each machine keeps its own: a scale that suits a laptop's screen is
//! wrong for a studio monitor.
//!
//! - **In the desktop app** the webview itself is zoomed (`setZoom` on the app's
//!   global, `host.rs`), as a browser's own zoom does: the device pixel ratio follows, so
//!   pictures and text stay sharp and pointer coordinates need no conversion.
//! - **In a browser** the page can't set the browser's zoom, so the root element gets CSS
//!   `zoom`. Lengths inside it are then drawn `zoom` times larger, while pointer events,
//!   `getBoundingClientRect` and the window's size stay in the viewport's pixels.
//!   [`css`] converts those to the root's CSS pixels wherever code places an element
//!   with them, and [`pixel_ratio`] gives the device pixels per CSS pixel the preview
//!   streams are sized by, so the canvas and the monitors stay sharp at every scale. The
//!   style sheet divides viewport units by `--ui-scale` for the same reason.
//!
//! Projector windows aren't scaled: they show a picture.
//!
//! Docs: docs/UI-FEATURES.md#header, crates/jstream-ui/README.md#design

use leptos::prelude::*;
use std::cell::Cell;

/// The scales offered, in percent, smallest first.
pub const STEPS: [u16; 9] = [50, 67, 75, 80, 90, 100, 110, 125, 150];

/// The scale a machine starts at.
pub const DEFAULT: u16 = 100;

/// Where the browser keeps the scale.
const KEY: &str = "jstream.ui-scale";

thread_local! {
    /// The CSS zoom on the root element now: 1 at 100 % and in the desktop app, whose
    /// webview zooms instead.
    static CSS_ZOOM: Cell<f64> = const { Cell::new(1.0) };
}

/// A kept value as a scale: one of [`STEPS`], else `None`.
pub fn parse(kept: &str) -> Option<u16> {
    kept.trim()
        .parse::<u16>()
        .ok()
        .filter(|scale| STEPS.contains(scale))
}

/// The step after `scale` towards larger (`up`) or smaller, or `scale` itself at the
/// end of [`STEPS`]. A scale between steps moves to the nearest step that way.
pub fn step(scale: u16, up: bool) -> u16 {
    if up {
        STEPS.into_iter().find(|&s| s > scale).unwrap_or(scale)
    } else {
        STEPS
            .into_iter()
            .rev()
            .find(|&s| s < scale)
            .unwrap_or(scale)
    }
}

/// What a scale reads as in the menu: "100%".
pub fn label(scale: u16) -> String {
    format!("{scale}%")
}

/// What Ctrl and a key do to the scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// One step larger.
    Larger,
    /// One step smaller.
    Smaller,
    /// Back to 100 %.
    Reset,
}

/// The scale key a keyboard event's `code` presses with Ctrl (or ⌘ on macOS), and
/// neither Alt nor Shift beyond what the key itself needs: `Equal` and `NumpadAdd`
/// enlarge, `Minus` and `NumpadSubtract` shrink, `Digit0` and `Numpad0` reset.
pub fn key(command: bool, alt: bool, code: &str) -> Option<Key> {
    if !command || alt {
        return None;
    }
    match code {
        "Equal" | "NumpadAdd" => Some(Key::Larger),
        "Minus" | "NumpadSubtract" => Some(Key::Smaller),
        "Digit0" | "Numpad0" => Some(Key::Reset),
        _ => None,
    }
}

/// The scale after `key`.
pub fn after(scale: u16, key: Key) -> u16 {
    match key {
        Key::Larger => step(scale, true),
        Key::Smaller => step(scale, false),
        Key::Reset => DEFAULT,
    }
}

/// The root's CSS pixels for `viewport` pixels (a pointer's `clientX`, a rectangle's
/// side, the window's width): the same number at 100 % and in the desktop app.
pub fn css(viewport: f64) -> f64 {
    viewport / CSS_ZOOM.with(Cell::get)
}

/// [`css`] for an `f32`.
pub fn css32(viewport: f32) -> f32 {
    css(f64::from(viewport)) as f32
}

/// An element's box in the root's CSS pixels: `getBoundingClientRect` through [`css`],
/// with the same accessors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    left: f64,
    top: f64,
    width: f64,
    height: f64,
}

impl Rect {
    /// The left edge.
    pub fn left(&self) -> f64 {
        self.left
    }
    /// The top edge.
    pub fn top(&self) -> f64 {
        self.top
    }
    /// The right edge.
    pub fn right(&self) -> f64 {
        self.left + self.width
    }
    /// The bottom edge.
    pub fn bottom(&self) -> f64 {
        self.top + self.height
    }
    /// The width.
    pub fn width(&self) -> f64 {
        self.width
    }
    /// The height.
    pub fn height(&self) -> f64 {
        self.height
    }
}

/// `element`'s box in the root's CSS pixels.
pub fn rect(element: &leptos::web_sys::Element) -> Rect {
    let r = element.get_bounding_client_rect();
    Rect {
        left: css(r.left()),
        top: css(r.top()),
        width: css(r.width()),
        height: css(r.height()),
    }
}

/// A pointer's position in the root's CSS pixels.
pub fn pointer(event: &leptos::web_sys::MouseEvent) -> (f64, f64) {
    (
        css(f64::from(event.client_x())),
        css(f64::from(event.client_y())),
    )
}

/// The window's inner size in the root's CSS pixels. Zero when the page has no size yet.
pub fn window_size() -> (f64, f64) {
    let size = |value: Result<leptos::wasm_bindgen::JsValue, _>| {
        css(value.ok().and_then(|value| value.as_f64()).unwrap_or(0.0))
    };
    (size(window().inner_width()), size(window().inner_height()))
}

/// Device pixels per CSS pixel inside the scaled page: the window's device pixel ratio,
/// at least 1, times the CSS zoom, so a picture sized by it fills the screen's pixels
/// and no more. Under 100 % that is below 1.
pub fn pixel_ratio() -> f64 {
    window().device_pixel_ratio().max(1.0) * CSS_ZOOM.with(Cell::get)
}

/// The scale the browser kept, else [`DEFAULT`]. Storage can be off.
pub fn kept() -> u16 {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(KEY).ok().flatten())
        .and_then(|kept| parse(&kept))
        .unwrap_or(DEFAULT)
}

/// Keeps `scale` in the browser's storage, when it has storage.
fn keep(scale: u16) {
    if let Ok(Some(storage)) = window().local_storage() {
        let _ = storage.set_item(KEY, &scale.to_string());
    }
}

/// Draws the page at `scale`: the desktop app's webview zooms when it can, else the root
/// element does.
fn apply(scale: u16) {
    let factor = f64::from(scale) / 100.0;
    let zoomed_by_app = crate::host::set_desktop_zoom(factor);
    let zoom = if zoomed_by_app { 1.0 } else { factor };
    CSS_ZOOM.with(|cell| cell.set(zoom));
    let Some(root) = document().document_element() else {
        return;
    };
    let Ok(root) = leptos::wasm_bindgen::JsCast::dyn_into::<leptos::web_sys::HtmlElement>(root)
    else {
        return;
    };
    let declarations = root.style();
    if zoom == 1.0 {
        let _ = declarations.remove_property("zoom");
        let _ = declarations.remove_property("--ui-scale");
    } else {
        let _ = declarations.set_property("zoom", &zoom.to_string());
        let _ = declarations.set_property("--ui-scale", &zoom.to_string());
    }
    // The canvas and the monitors size their streams when their stage resizes; a zoom
    // that leaves the stage's CSS size alone still changes its device pixels.
    if let Ok(event) = leptos::web_sys::Event::new("resize") {
        let _ = window().dispatch_event(&event);
    }
}

/// The page's UI scale, in percent, shared by the View menu and the keys.
#[derive(Clone, Copy)]
pub struct UiScale(pub RwSignal<u16>);

impl UiScale {
    /// Provides the kept scale and draws the page at it, now and at each change, which is
    /// kept, and listens for the keys (a chord the project binds as a hotkey is left to
    /// it). Called once, by the app, on every page but a projector's.
    pub fn provide() -> UiScale {
        let scale = UiScale(RwSignal::new(kept()));
        apply(scale.0.get_untracked());
        Effect::new(move |previous: Option<u16>| {
            let now = scale.0.get();
            if previous.is_some_and(|previous| previous != now) {
                apply(now);
                keep(now);
            }
            now
        });
        let studio = use_context::<crate::store::Studio>();
        let keys = window_event_listener(leptos::ev::keydown, move |event| {
            if event.default_prevented() || event.repeat() {
                return;
            }
            let command = event.ctrl_key() || event.meta_key();
            let Some(key) = key(command, event.alt_key(), &event.code()) else {
                return;
            };
            // A hotkey the project binds to the chord runs instead (`hotkeys.rs`).
            let bound = crate::hotkeys::of_event(&event).is_some_and(|chord| {
                studio.is_some_and(|studio| {
                    studio
                        .collection
                        .with_untracked(|c| c.as_ref().is_some_and(|c| c.hotkey(&chord).is_some()))
                })
            });
            if bound {
                return;
            }
            // The browser's own zoom would apply on top.
            event.prevent_default();
            scale.0.update(|scale| *scale = after(*scale, key));
        });
        on_cleanup(move || keys.remove());
        provide_context(scale);
        scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_run_from_half_to_one_and_a_half_through_the_default() {
        assert_eq!(STEPS.first(), Some(&50));
        assert_eq!(STEPS.last(), Some(&150));
        assert!(STEPS.contains(&DEFAULT));
        assert!(STEPS.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn a_step_moves_to_the_next_scale_and_stops_at_the_ends() {
        assert_eq!(step(100, true), 110);
        assert_eq!(step(100, false), 90);
        assert_eq!(step(150, true), 150);
        assert_eq!(step(50, false), 50);
        // Between steps, the nearest one that way.
        assert_eq!(step(105, true), 110);
        assert_eq!(step(105, false), 100);
    }

    #[test]
    fn only_an_offered_scale_is_read_back() {
        assert_eq!(parse("125"), Some(125));
        assert_eq!(parse(" 67 "), Some(67));
        assert_eq!(parse("120"), None);
        assert_eq!(parse("big"), None);
        assert_eq!(label(80), "80%");
    }

    #[test]
    fn ctrl_with_equals_minus_or_zero_scales() {
        assert_eq!(key(true, false, "Equal"), Some(Key::Larger));
        assert_eq!(key(true, false, "NumpadAdd"), Some(Key::Larger));
        assert_eq!(key(true, false, "Minus"), Some(Key::Smaller));
        assert_eq!(key(true, false, "Digit0"), Some(Key::Reset));
        assert_eq!(key(false, false, "Equal"), None);
        assert_eq!(key(true, true, "Equal"), None);
        assert_eq!(key(true, false, "KeyZ"), None);
        assert_eq!(after(125, Key::Larger), 150);
        assert_eq!(after(67, Key::Smaller), 50);
        assert_eq!(after(50, Key::Reset), 100);
    }

    #[test]
    fn at_full_size_viewport_pixels_are_css_pixels() {
        assert_eq!(css(120.0), 120.0);
        assert_eq!(css32(7.5), 7.5);
    }
}
