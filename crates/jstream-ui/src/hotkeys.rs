//! Hotkeys in the page (ADR-0024 §5): the collection's bindings, run while the page has
//! the focus. The engine stores the bindings and never listens for keys; the web UI
//! listens for in-page ones. A key press that a field or a focused row already handled
//! (its default prevented, or typed in a field) is left alone, so F2 still renames a
//! focused scene row. A scene's chords are bound from the Inspector's Hotkey row: the
//! chord pressed there is bound to `scene.switch` of the scene, taking it from whatever
//! it ran before. The scene's Properties dialog also sets its first chord, and refuses
//! one bound to something else.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#keyboard

use crate::app::SessionContext;
use crate::store::{Collection, Studio};
use jstream_control::protocol::{Action, Chord, Edit, SceneId};
use leptos::prelude::*;

/// The chord for a key press: its modifiers, in the protocol's order, then the
/// physical key (`KeyboardEvent.code`). `None` for a modifier key alone, or a key a
/// chord can't name.
pub fn chord(ctrl: bool, alt: bool, shift: bool, meta: bool, code: &str) -> Option<Chord> {
    if !Chord::KEYS.contains(&code) {
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

/// The edits that bind `chord` to putting `scene_id` on program: a `hotkey.clear` of
/// whatever the chord ran before, then the `hotkey.set`. Empty when it already does.
pub fn bind_edits(collection: &Collection, scene_id: &SceneId, chord: &Chord) -> Vec<Edit> {
    let bound = collection.hotkeys.iter().find(|b| &b.chord == chord);
    if bound.is_some_and(|b| b.action() == Some(switch_to(scene_id))) {
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
        action: switch_to(scene_id),
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
    on_cleanup(move || keys.remove());
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
            bind_edits(&collection, &a, &f1),
            vec![Edit::HotkeySet {
                chord: f1.clone(),
                action: switch_to(&a),
            }]
        );
        collection.hotkeys = vec![HotkeyBinding::new(f1.clone(), &switch_to(&b))];
        assert_eq!(
            bind_edits(&collection, &a, &f1).len(),
            2,
            "cleared, then set"
        );
        assert!(bind_edits(&collection, &b, &f1).is_empty(), "already bound");
        assert_eq!(collection.hotkey(&f1), Some(switch_to(&b)));
        assert_eq!(collection.scene_chords(&b), vec![f1]);
        assert!(collection.scene_chords(&a).is_empty());
    }
}
