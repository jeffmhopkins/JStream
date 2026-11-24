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

/// What a bound wheel chord's chip says on macOS when the desktop app hears wheel chords
/// only while its window has the focus ([`crate::host::desktop_wheel_focus_only`]):
/// macOS asks for the Accessibility permission before an app may watch the wheel
/// elsewhere.
pub const WHEEL_FOCUS_ONLY: &str = "Heard only while JStream has the focus. To hear it in other apps, allow JStream in System Settings › Privacy & Security › Accessibility, then restart JStream.";

/// What it says elsewhere: on Wayland, whose GlobalShortcuts portal takes keys but not
/// the wheel, or when no hotkey listener started.
pub const WHEEL_FOCUS_ONLY_HERE: &str = "Heard only while JStream has the focus: on this desktop, apps can't hear the wheel elsewhere. To zoom from another app, bind keys.";

/// The note for `chord`'s chip, if it has one: for a wheel chord when `focus_only`,
/// [`WHEEL_FOCUS_ONLY`] on macOS (`macos`) and [`WHEEL_FOCUS_ONLY_HERE`] elsewhere.
pub fn chord_note(chord: &Chord, focus_only: bool, macos: bool) -> Option<&'static str> {
    (focus_only && chord.is_wheel()).then_some(if macos {
        WHEEL_FOCUS_ONLY
    } else {
        WHEEL_FOCUS_ONLY_HERE
    })
}

/// How far the wheel turns for one notch, by `WheelEvent.deltaMode`: in pixels, 120 as
/// Chromium reports a notch on Linux at every devicePixelRatio (measured, #1346) and
/// near the 100 it reports on Windows; 3 lines in browsers that report lines; one page.
const PIXELS_PER_NOTCH: f64 = 120.0;
const LINES_PER_NOTCH: f64 = 3.0;

/// Counts wheel events as notches. An event of half a notch or more comes from a wheel
/// that clicks, so it counts on its own, rounded, at least one: a notch is one step
/// whether the browser reports it as 100 or 120 pixels, and an event the browser
/// coalesced from several notches counts each. Smaller events, from a smooth wheel or a
/// touchpad, add up, carrying what's left over to the next event. A fast touchpad
/// swipe whose events are each 60 pixels or more counts every event as a notch, as a
/// wheel's would, rather than summing them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelNotches {
    /// The small turns not yet counted, in notches: negative is up.
    carry: f64,
}

impl WheelNotches {
    /// Adds one wheel event's vertical turn ([`wheel_delta`], in `deltaMode`'s unit) and
    /// returns the whole notches it completes: negative turns up (away from the person),
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
        let whole = if turn.abs() >= 0.5 {
            // A wheel that clicks: this event alone.
            self.carry = 0.0;
            turn.round()
        } else {
            if self.carry * turn < 0.0 {
                self.carry = 0.0;
            }
            self.carry += turn;
            // Six 20-pixel deltas are a whole notch, though the sum of sixths falls
            // just short of 1 in floating point.
            let whole = (self.carry + self.carry.signum() * 1e-9).trunc();
            self.carry -= whole;
            whole
        };
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

/// The wheel chord a turn of the wheel names, for a hotkey field taking the next chord
/// (ADR-0112 §3): `delta` is [`wheel_delta`]'s, negative for up. None for a turn with no
/// modifier held, which scrolls as usual: bound, a bare `WheelUp` would take every
/// scroll of the page.
pub fn wheel_chord(ctrl: bool, alt: bool, shift: bool, meta: bool, delta: f64) -> Option<Chord> {
    let direction = if delta < 0.0 {
        "WheelUp"
    } else if delta > 0.0 {
        "WheelDown"
    } else {
        return None;
    };
    (ctrl || alt || shift || meta)
        .then(|| chord(ctrl, alt, shift, meta, direction))
        .flatten()
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
        via: Vec::new(),
    }
}

/// The default zoom bindings for `scene_id`'s follow camera (ADR-0112 §4):
/// `Ctrl+WheelUp` zooms in a step and `Ctrl+WheelDown` out. None when either chord is
/// bound already, to anything, so the offer never takes a chord from another action.
pub fn default_zoom_edits(collection: &Collection, scene_id: &SceneId) -> Option<Vec<Edit>> {
    let up = Chord::new("Ctrl+WheelUp");
    let down = Chord::new("Ctrl+WheelDown");
    if collection.hotkey(&up).is_some() || collection.hotkey(&down).is_some() {
        return None;
    }
    let zoom = |chord: Chord, steps: i32| Edit::HotkeySet {
        chord,
        action: Action::SceneZoomCamera {
            scene_id: scene_id.clone(),
            steps,
        },
    };
    Some(vec![zoom(up, 1), zoom(down, -1)])
}

/// The edits that bind `chord` to `action` (a scene's [`switch_to`], an item's
/// `item.toggle`, `item.show` or `item.hide`, or any other action a row binds). Empty
/// when the chord runs it already. A chord runs several actions (ADR-0208), so the
/// chord keeps every other action, except one `action` would fight: another scene's
/// switch, or another of the same item's show, hide or toggle, each unbound by name
/// (`hotkey.clear` with its `action`) before the `hotkey.set`.
pub fn bind_edits(collection: &Collection, action: &Action, chord: &Chord) -> Vec<Edit> {
    let bound = collection.hotkey_actions(chord);
    if bound.contains(action) {
        return Vec::new();
    }
    let mut edits: Vec<Edit> = bound
        .into_iter()
        .filter(|old| conflicts(old, action))
        .map(|old| Edit::HotkeyClear {
            chord: chord.clone(),
            action: Some(old),
        })
        .collect();
    edits.push(Edit::HotkeySet {
        chord: chord.clone(),
        action: action.clone(),
    });
    edits
}

/// Whether one press running both `a` and `b` would undo itself: two scene switches,
/// or two of one item's show, hide and toggle.
fn conflicts(a: &Action, b: &Action) -> bool {
    let item = |action: &Action| match action {
        Action::ItemToggle { item_id, scene_id }
        | Action::ItemShow { item_id, scene_id }
        | Action::ItemHide { item_id, scene_id } => Some((item_id.clone(), scene_id.clone())),
        _ => None,
    };
    match (a, b) {
        (Action::SceneSwitch { .. }, Action::SceneSwitch { .. }) => true,
        _ => item(a).is_some_and(|ia| item(b) == Some(ia)),
    }
}

/// Whether a key press belongs to what has the focus: a text field, a select or an
/// editable element types it.
pub(crate) fn typed_in_field(event: &leptos::ev::KeyboardEvent) -> bool {
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
pub(crate) fn overlay_open() -> bool {
    leptos::prelude::document()
        .query_selector("[aria-modal='true'], [role='menu']")
        .ok()
        .flatten()
        .is_some()
}

/// Runs the collection's hotkeys while the page has the focus: a bound chord sends each
/// of its actions, in the order they were bound (ADR-0208), as `action` calls, and the browser's own use of the key (F5 reloads) is
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
        let actions = studio
            .collection
            .with_untracked(|c| c.as_ref().map(|c| c.hotkey_actions(&chord)))
            .unwrap_or_default();
        if actions.is_empty() {
            return;
        }
        event.prevent_default();
        if let Some(s) = session.get() {
            for action in actions {
                studio.act(&s, action);
            }
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
            let actions = studio
                .collection
                .with_untracked(|c| c.as_ref().map(|c| c.hotkey_actions(&chord)))
                .unwrap_or_default();
            (!actions.is_empty()).then_some(actions)
        };
        let delta = wheel_delta(event.delta_x(), event.delta_y(), modifiers.2);
        let direction = if delta < 0.0 {
            "WheelUp"
        } else if delta > 0.0 {
            "WheelDown"
        } else {
            return;
        };
        let Some(actions) = bound(direction) else {
            // Not bound this way: the page scrolls or zooms as it would.
            notches.reset();
            return;
        };
        // Bound with these modifiers, this way: the page doesn't scroll or zoom too.
        event.prevent_default();
        let turned = notches.turn(delta, event.delta_mode());
        if let Some(s) = session.get() {
            for _ in 0..turned.unsigned_abs() {
                for action in &actions {
                    studio.act(&s, action.clone());
                }
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

    /// A hotkey field takes a turn of the wheel with a modifier as a wheel chord, and
    /// leaves a bare turn to scroll.
    #[test]
    fn a_turn_with_a_modifier_names_a_wheel_chord() {
        assert_eq!(
            wheel_chord(true, false, false, false, -100.0),
            Some(Chord::new("Ctrl+WheelUp"))
        );
        assert_eq!(
            wheel_chord(false, true, true, false, 3.0),
            Some(Chord::new("Alt+Shift+WheelDown"))
        );
        assert_eq!(wheel_chord(false, false, false, false, -100.0), None);
        assert_eq!(wheel_chord(true, false, false, false, 0.0), None);
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
        // One notch as Chromium reports it on Linux (120 px, at a devicePixelRatio of 1,
        // 1.5 and 2) and on Windows (100 px), in lines and in pages.
        assert_eq!(notches.turn(120.0, 0), 1);
        assert_eq!(notches.turn(-120.0, 0), -1);
        assert_eq!(notches.turn(100.0, 0), 1);
        assert_eq!(notches.turn(-100.0, 0), -1);
        assert_eq!(notches.turn(3.0, 1), 1);
        assert_eq!(notches.turn(1.0, 2), 1);
        // Five notches are five steps, at either size.
        let five = |px: f64, notches: &mut WheelNotches| -> i32 {
            (0..5).map(|_| notches.turn(px, 0)).sum()
        };
        assert_eq!(five(120.0, &mut notches), 5);
        assert_eq!(five(100.0, &mut notches), 5);
        // Notches the browser coalesced into one event count each.
        assert_eq!(notches.turn(240.0, 0), 2);
        assert_eq!(notches.turn(360.0, 0), 3);
        assert_eq!(notches.turn(-300.0, 0), -3);
        // A smooth wheel's or a touchpad's small deltas add up to one notch, and the
        // rest carries.
        notches.reset();
        assert_eq!(notches.turn(40.0, 0), 0);
        assert_eq!(notches.turn(40.0, 0), 0);
        assert_eq!(notches.turn(40.0, 0), 1);
        assert_eq!(notches.turn(-1.0, 0), 0, "turning back drops the carry");
        assert_eq!(notches.turn(-50.0, 0), 0);
        assert_eq!(notches.turn(-50.0, 0), 0);
        assert_eq!(notches.turn(-50.0, 0), -1);
        // Six of a touchpad's 20-pixel deltas are one notch exactly.
        notches.reset();
        let six: i32 = (0..6).map(|_| notches.turn(20.0, 0)).sum();
        assert_eq!(six, 1);
        // A notch-sized event drops what was carried.
        assert_eq!(notches.turn(-50.0, 0), 0);
        assert_eq!(notches.turn(-120.0, 0), -1);
        assert_eq!(notches.turn(-50.0, 0), 0);
        assert_eq!(
            notches.turn(-50.0, 0),
            0,
            "the -50 before the notch is gone"
        );
        assert_eq!(notches.turn(0.0, 0), 0);
        assert_eq!(notches.turn(f64::NAN, 0), 0);
    }

    #[test]
    fn only_a_wheel_chord_is_noted_and_only_when_the_app_says_so() {
        let wheel = Chord::new("Ctrl+WheelUp");
        let key = Chord::new("Ctrl+KeyZ");
        assert_eq!(chord_note(&wheel, true, true), Some(WHEEL_FOCUS_ONLY));
        assert_eq!(chord_note(&wheel, true, false), Some(WHEEL_FOCUS_ONLY_HERE));
        assert_eq!(chord_note(&wheel, false, true), None);
        assert_eq!(chord_note(&key, true, false), None);
    }

    #[test]
    fn labels_drop_the_key_code_prefixes() {
        assert_eq!(label(&Chord::new("Ctrl+KeyA")), "Ctrl+A");
        assert_eq!(label(&Chord::new("Shift+Digit1")), "Shift+1");
        assert_eq!(label(&Chord::new("F5")), "F5");
        assert_eq!(label(&Chord::new("Numpad1")), "Numpad1");
    }

    /// The default zoom chords are offered only while neither is bound.
    #[test]
    fn the_default_zoom_chords_are_offered_while_neither_is_bound() {
        let a = SceneId::new("a");
        let mut collection = Collection::default();
        let edits = default_zoom_edits(&collection, &a).unwrap_or_default();
        assert_eq!(
            edits,
            vec![
                Edit::HotkeySet {
                    chord: Chord::new("Ctrl+WheelUp"),
                    action: Action::SceneZoomCamera {
                        scene_id: a.clone(),
                        steps: 1,
                    },
                },
                Edit::HotkeySet {
                    chord: Chord::new("Ctrl+WheelDown"),
                    action: Action::SceneZoomCamera {
                        scene_id: a.clone(),
                        steps: -1,
                    },
                },
            ]
        );
        collection.hotkeys = vec![HotkeyBinding::new(
            Chord::new("Ctrl+WheelDown"),
            &Action::SceneZoomCamera {
                scene_id: SceneId::new("b"),
                steps: -1,
            },
        )];
        assert_eq!(default_zoom_edits(&collection, &a), None);
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
            "the old switch unbound, then set"
        );
        assert!(
            bind_edits(&collection, &switch_to(&b), &f1).is_empty(),
            "already bound"
        );
        assert_eq!(collection.hotkey(&f1), Some(switch_to(&b)));
        assert_eq!(collection.scene_chords(&b), vec![f1]);
        assert!(collection.scene_chords(&a).is_empty());
    }

    /// A chord running several actions (ADR-0208) keeps the ones the new action doesn't
    /// fight: re-binding an action it runs is nothing, and another item's toggle is added.
    #[test]
    fn binding_keeps_a_chords_other_actions() {
        let f9 = Chord::new("F9");
        let live = Action::OutputToggle {
            output_id: jstream_control::protocol::OutputId::new("yt"),
        };
        let cam = Action::ItemToggle {
            item_id: jstream_control::protocol::ItemId::new("cam"),
            scene_id: None,
        };
        let mic = Action::ItemToggle {
            item_id: jstream_control::protocol::ItemId::new("mic"),
            scene_id: None,
        };
        let a = SceneId::new("a");
        let b = SceneId::new("b");
        let collection = Collection {
            hotkeys: vec![
                HotkeyBinding::new(f9.clone(), &live),
                HotkeyBinding::new(f9.clone(), &cam),
                HotkeyBinding::new(f9.clone(), &switch_to(&b)),
            ],
            ..Collection::default()
        };
        assert!(
            bind_edits(&collection, &cam, &f9).is_empty(),
            "runs it already"
        );
        assert_eq!(
            bind_edits(&collection, &mic, &f9),
            vec![Edit::HotkeySet {
                chord: f9.clone(),
                action: mic,
            }]
        );
        assert_eq!(
            bind_edits(&collection, &switch_to(&a), &f9),
            vec![
                Edit::HotkeyClear {
                    chord: f9.clone(),
                    action: Some(switch_to(&b)),
                },
                Edit::HotkeySet {
                    chord: f9,
                    action: switch_to(&a),
                },
            ],
            "only the other switch goes"
        );
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
