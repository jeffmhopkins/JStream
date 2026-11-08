//! Hotkeys in the page (ADR-0024 §5): the collection's bindings, run while the page has
//! the focus. The engine stores the bindings and never listens for keys; the web UI
//! listens for in-page ones. A key press that a field or a focused row already handled
//! (its default prevented, or typed in a field) is left alone, so F2 still renames a
//! focused scene row. A scene's chords are bound from the Inspector's Hotkey row: the
//! chord pressed there is bound to `scene.switch` of the scene, taking it from whatever
//! it ran before. The scene's Properties dialog also sets its first chord, and refuses
//! one bound to something else. An item's Hotkeys section binds chords the same way to
//! `item.toggle`, `item.show` and `item.hide` of the item (ADR-0101), which show and hide
//! it with its own appear and disappear effects.
//!
//! A chord may end in the wheel (`Ctrl+WheelUp`, ADR-0112 §3): while the page has the
//! focus, each notch of a bound wheel chord runs its action once ([`WheelNotches`]),
//! and the browser's own use of it (Ctrl+wheel zooms the page) is prevented.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#keyboard

use crate::app::SessionContext;
use crate::store::{Collection, Studio};
use jstream_control::protocol::{Action, Chord, Edit, SceneId};
use leptos::prelude::*;

/// The chord for a key press: its modifiers, in the protocol's order, then the
/// physical key (`KeyboardEvent.code`) or the wheel's direction (`WheelUp`,
/// `WheelDown`). `None` for a modifier key alone, or a key a chord can't name.
pub fn chord(ctrl: bool, alt: bool, shift: bool, meta: bool, code: &str) -> Option<Chord> {
    if !Chord::KEYS.contains(&code) && !Chord::WHEEL.contains(&code) {
        return None;
    }
    let mut parts: Vec<&str> = [
        (ctrl, "Ctrl"),
        (alt, "Alt"),
        (shift, "Shift"),
        (meta, "Meta"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    parts.push(code);
    let chord = Chord::new(parts.join("+"));
    chord.problem().is_none().then_some(chord)
}

/// The chord a keyboard event presses.
pub fn of_event(event: &leptos::ev::KeyboardEvent) -> Option<Chord> {
    chord(
        event.ctrl_key(),
        event.alt_key(),
        event.shift_key(),
        event.meta_key(),
        &event.code(),
    )
}

/// How far the wheel turns for one notch, by `WheelEvent.deltaMode`: about 100 pixels
/// in the browsers that report pixels, 3 lines in those that report lines, and one page.
const PIXELS_PER_NOTCH: f64 = 100.0;
const LINES_PER_NOTCH: f64 = 3.0;

/// Turns wheel deltas into whole notches, carrying what's left over to the next event,
/// so a smooth wheel or a touchpad runs a wheel chord once per notch's worth of turn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelNotches {
    /// The turn not yet counted, in notches: negative is up.
    carry: f64,
}

impl WheelNotches {
    /// Adds one wheel event's vertical turn ([`wheel_delta`], in `deltaMode`'s unit) and returns
    /// the whole notches it completes: negative turns up (away from the person),
    /// positive down. A turn the other way drops what was carried.
    pub fn turn(&mut self, delta_y: f64, delta_mode: u32) -> i32 {
        let per_notch = match delta_mode {
            0 => PIXELS_PER_NOTCH,
            1 => LINES_PER_NOTCH,
            _ => 1.0,
        };
        let turn = delta_y / per_notch;
        if !turn.is_finite() || turn == 0.0 {
            return 0;
        }
        if self.carry * turn < 0.0 {
            self.carry = 0.0;
        }
        self.carry += turn;
        let whole = self.carry.trunc();
        self.carry -= whole;
        // At most a few notches per event: the cast can't overflow.
        whole.clamp(-64.0, 64.0) as i32
    }

    /// Forgets what was carried, when the wheel turns without a bound chord.
    pub fn reset(&mut self) {
        self.carry = 0.0;
    }
}

/// The vertical turn of a wheel event, in its `deltaMode`'s unit. Browsers report a
/// turn with Shift held as a sideways scroll, in `deltaX` with `deltaY` 0 (macOS
/// converts it too), so with Shift held and no vertical turn the sideways one counts.
pub fn wheel_delta(delta_x: f64, delta_y: f64, shift: bool) -> f64 {
    if shift && delta_y == 0.0 {
        delta_x
    } else {
        delta_y
    }
}

/// A chord as people read it: `Ctrl+A` for `Ctrl+KeyA`, `Shift+1` for `Shift+Digit1`.
pub fn label(chord: &Chord) -> String {
    chord
        .as_str()
        .split('+')
        .map(|part| {
            part.strip_prefix("Key")
                .or_else(|| part.strip_prefix("Digit"))
                .filter(|rest| rest.len() == 1)
                .unwrap_or(part)
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// The action that puts `scene_id` on program, as a scene's hotkey runs it.
pub fn switch_to(scene_id: &SceneId) -> Action {
    Action::SceneSwitch {
        scene_id: scene_id.clone(),
        preset_id: None,
    }
}

/// The edits that bind `chord` to `action` (a scene's [`switch_to`], or an item's
/// `item.toggle`, `item.show` or `item.hide`): a `hotkey.clear` of whatever the chord
/// ran before, then the `hotkey.set`. Empty when it already does.
pub fn bind_edits(collection: &Collection, action: &Action, chord: &Chord) -> Vec<Edit> {
    let bound = collection.hotkeys.iter().find(|b| &b.chord == chord);
    if bound.is_some_and(|b| b.action().as_ref() == Some(action)) {
        return Vec::new();
    }
    let mut edits = Vec::new();
    if bound.is_some() {
        edits.push(Edit::HotkeyClear {
            chord: chord.clone(),
        });
    }
    edits.push(Edit::HotkeySet {
        chord: chord.clone(),
        action: action.clone(),
    });
    edits
}

/// Whether a key press belongs to what has the focus: a text field, a select or an
/// editable element types it.
fn typed_in_field(event: &leptos::ev::KeyboardEvent) -> bool {
    use leptos::wasm_bindgen::JsCast;
    let Some(element) = event
        .target()
        .and_then(|t| t.dyn_into::<leptos::web_sys::HtmlElement>().ok())
    else {
        return false;
    };
    matches!(
        element.tag_name().to_ascii_lowercase().as_str(),
        "input" | "textarea" | "select"
    ) || element.is_content_editable()
}

/// Whether a dialog or a menu is open: it holds the keyboard, so no binding runs.
fn overlay_open() -> bool {
    leptos::prelude::document()
        .query_selector("[aria-modal='true'], [role='menu']")
        .ok()
        .flatten()
        .is_some()
}

/// Runs the collection's hotkeys while the page has the focus: a bound chord sends its
/// action, as an `action` call, and the browser's own use of the key (F5 reloads) is
/// prevented. Nothing runs while a dialog or a menu is open. Mounted once, in the shell.
#[component]
pub fn Hotkeys() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let keys = window_event_listener(leptos::ev::keydown, move |event| {
        if event.default_prevented() || event.repeat() || typed_in_field(&event) || overlay_open() {
            return;
        }
        let Some(chord) = of_event(&event) else {
            return;
        };
        let Some(action) = studio
            .collection
            .with_untracked(|c| c.as_ref().and_then(|c| c.hotkey(&chord)))
        else {
            return;
        };
        event.prevent_default();
        if let Some(s) = session.get() {
            studio.act(&s, action);
        }
    });
    let wheel = wheel_listener(move |event, notches| {
        if event.default_prevented() || overlay_open() {
            return;
        }
        let modifiers = (
            event.ctrl_key(),
            event.alt_key(),
            event.shift_key(),
            event.meta_key(),
        );
        let bound = |direction: &str| {
            let chord = chord(
                modifiers.0,
                modifiers.1,
                modifiers.2,
                modifiers.3,
                direction,
            )?;
            studio
                .collection
                .with_untracked(|c| c.as_ref().and_then(|c| c.hotkey(&chord)))
        };
        let delta = wheel_delta(event.delta_x(), event.delta_y(), modifiers.2);
        let direction = if delta < 0.0 {
            "WheelUp"
        } else if delta > 0.0 {
            "WheelDown"
        } else {
            return;
        };
        let Some(action) = bound(direction) else {
            // Not bound this way: the page scrolls or zooms as it would.
            notches.reset();
            return;
        };
        // Bound with these modifiers, this way: the page doesn't scroll or zoom too.
        event.prevent_default();
        let turned = notches.turn(delta, event.delta_mode());
        if let Some(s) = session.get() {
            for _ in 0..turned.unsigned_abs() {
                studio.act(&s, action.clone());
            }
        }
    });
    on_cleanup(move || {
        keys.remove();
        wheel.remove();
    });
}

/// A `wheel` listener on the window that may prevent the default: browsers treat a
/// window's wheel listeners as passive unless told otherwise, and a passive one can't
/// stop Ctrl+wheel zooming the page. `on_wheel` gets each event and the listener's
/// [`WheelNotches`].
fn wheel_listener(
    on_wheel: impl Fn(&leptos::web_sys::WheelEvent, &mut WheelNotches) + 'static,
) -> WheelListener {
    use leptos::wasm_bindgen::{JsCast, closure::Closure};
    let mut notches = WheelNotches::default();
    let callback = Closure::<dyn FnMut(leptos::web_sys::WheelEvent)>::new(
        move |event: leptos::web_sys::WheelEvent| on_wheel(&event, &mut notches),
    )
    .into_js_value();
    let options = leptos::web_sys::AddEventListenerOptions::new();
    options.set_passive(false);
    let _ = window().add_event_listener_with_callback_and_add_event_listener_options(
        "wheel",
        callback.unchecked_ref(),
        &options,
    );
    WheelListener(StoredValue::new_local(callback))
}

/// The window's wheel listener, until [`WheelListener::remove`].
#[derive(Clone, Copy)]
struct WheelListener(StoredValue<leptos::wasm_bindgen::JsValue, LocalStorage>);

impl WheelListener {
    fn remove(self) {
        use leptos::wasm_bindgen::JsCast;
        self.0.with_value(|callback| {
            let _ = window().remove_event_listener_with_callback("wheel", callback.unchecked_ref());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::HotkeyBinding;

    #[test]
    fn chords_follow_the_protocol_spelling() {
        assert_eq!(
            chord(false, false, false, false, "F1"),
            Some(Chord::new("F1"))
        );
        assert_eq!(
            chord(true, false, true, false, "Digit1"),
            Some(Chord::new("Ctrl+Shift+Digit1"))
        );
        assert_eq!(
            chord(true, true, true, true, "KeyA"),
            Some(Chord::new("Ctrl+Alt+Shift+Meta+KeyA"))
        );
        assert_eq!(chord(true, false, false, false, "ControlLeft"), None);
        assert_eq!(chord(false, false, false, false, "IntlRo"), None);
        assert_eq!(
            chord(true, false, false, false, "WheelUp"),
            Some(Chord::new("Ctrl+WheelUp"))
        );
        assert_eq!(
            chord(false, true, true, false, "WheelDown"),
            Some(Chord::new("Alt+Shift+WheelDown"))
        );
    }

    /// With Shift held, browsers report the wheel sideways: that turn counts.
    #[test]
    fn a_shift_wheel_turn_reads_the_sideways_delta() {
        assert_eq!(wheel_delta(0.0, -100.0, false), -100.0);
        assert_eq!(wheel_delta(0.0, -100.0, true), -100.0);
        assert_eq!(wheel_delta(100.0, 0.0, true), 100.0);
        assert_eq!(wheel_delta(-100.0, 0.0, true), -100.0);
        // Without Shift, a sideways scroll isn't a turn of the wheel.
        assert_eq!(wheel_delta(100.0, 0.0, false), 0.0);
    }

    #[test]
    fn wheel_deltas_count_whole_notches() {
        let mut notches = WheelNotches::default();
        // One notch in pixels, lines and pages.
        assert_eq!(notches.turn(-100.0, 0), -1);
        assert_eq!(notches.turn(3.0, 1), 1);
        assert_eq!(notches.turn(1.0, 2), 1);
        // A smooth wheel's small deltas add up to one notch, and the rest carries.
        assert_eq!(notches.turn(40.0, 0), 0);
        assert_eq!(notches.turn(40.0, 0), 0);
        assert_eq!(notches.turn(40.0, 0), 1);
        assert_eq!(notches.turn(80.0, 0), 1);
        // Turning back drops what was carried.
        assert_eq!(notches.turn(60.0, 0), 0);
        assert_eq!(notches.turn(-60.0, 0), 0);
        assert_eq!(notches.turn(-40.0, 0), -1);
        // A fast flick is several notches.
        notches.reset();
        assert_eq!(notches.turn(300.0, 0), 3);
        assert_eq!(notches.turn(0.0, 0), 0);
        assert_eq!(notches.turn(f64::NAN, 0), 0);
    }

    #[test]
    fn labels_drop_the_key_code_prefixes() {
        assert_eq!(label(&Chord::new("Ctrl+KeyA")), "Ctrl+A");
        assert_eq!(label(&Chord::new("Shift+Digit1")), "Shift+1");
        assert_eq!(label(&Chord::new("F5")), "F5");
        assert_eq!(label(&Chord::new("Numpad1")), "Numpad1");
    }

    #[test]
    fn binding_takes_the_chord_from_its_old_action() {
        let a = SceneId::new("a");
        let b = SceneId::new("b");
        let f1 = Chord::new("F1");
        let mut collection = Collection::default();
        assert_eq!(
            bind_edits(&collection, &switch_to(&a), &f1),
            vec![Edit::HotkeySet {
                chord: f1.clone(),
                action: switch_to(&a),
            }]
        );
        collection.hotkeys = vec![HotkeyBinding::new(f1.clone(), &switch_to(&b))];
        assert_eq!(
            bind_edits(&collection, &switch_to(&a), &f1).len(),
            2,
            "cleared, then set"
        );
        assert!(
            bind_edits(&collection, &switch_to(&b), &f1).is_empty(),
            "already bound"
        );
        assert_eq!(collection.hotkey(&f1), Some(switch_to(&b)));
        assert_eq!(collection.scene_chords(&b), vec![f1]);
        assert!(collection.scene_chords(&a).is_empty());
    }

    #[test]
    fn an_items_chords_are_the_ones_bound_to_its_action() {
        let toggle = Action::ItemToggle {
            item_id: jstream_control::protocol::ItemId::new("i1"),
            scene_id: None,
        };
        let show = Action::ItemShow {
            item_id: jstream_control::protocol::ItemId::new("i1"),
            scene_id: None,
        };
        let f2 = Chord::new("F2");
        let collection = Collection {
            hotkeys: vec![HotkeyBinding::new(f2.clone(), &toggle)],
            ..Collection::default()
        };
        assert_eq!(collection.chords_for(&toggle), vec![f2.clone()]);
        assert!(collection.chords_for(&show).is_empty());
        assert!(
            bind_edits(&collection, &toggle, &f2).is_empty(),
            "already bound"
        );
        assert_eq!(
            bind_edits(&collection, &show, &f2).len(),
            2,
            "taken from the toggle"
        );
    }
}
