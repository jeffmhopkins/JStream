//! Hotkey bindings: a key chord bound to a protocol action, kept in the collection
//! ([ADR-0024 §5](../../../docs/adr/0024-scene-structure.md), with the deleted-target rule
//! of [ADR-0040 §3](../../../docs/adr/0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)).
//!
//! The engine stores bindings and checks them; it never listens for keys. Who does (the
//! desktop host for OS-global shortcuts, the web UI for in-page ones, nobody in the
//! headless daemon) is a client decision. Every binding runs a protocol action, so a
//! hotkey can do nothing an agent can't (R-ARCH-001).

use crate::command::Action;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A key chord: zero or more modifiers then one key, joined by `+`, such as `Ctrl+KeyA`,
/// `Ctrl+Shift+Digit1`, `F1` or `Pause`.
///
/// - The modifiers are `Ctrl`, `Alt`, `Shift` and `Meta` (the Windows or Command key),
///   each at most once, in that order. A chord has exactly one spelling, so two chords
///   are the same keys exactly when their strings are equal.
/// - The key is a W3C UI Events `KeyboardEvent.code` value from [`Chord::KEYS`]: the
///   physical key, whatever the keyboard layout, as browsers and the desktop host's
///   global-shortcut API both report it. Modifier keys themselves are not keys here.
///
/// On the wire it is a plain string. The protocol layer doesn't check it: the engine
/// rejects one that isn't valid with `invalid_argument`, giving [`Chord::problem`]'s
/// reason.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Chord(String);

impl Chord {
    /// The modifiers, in the order a chord must list them.
    pub const MODIFIERS: [&'static str; 4] = ["Ctrl", "Alt", "Shift", "Meta"];

    /// The keys a chord may end in: W3C UI Events `KeyboardEvent.code` values for the
    /// writing-system keys, digits, function keys, editing and navigation keys, the
    /// numeric keypad, and the media keys.
    pub const KEYS: &'static [&'static str] = &[
        "KeyA",
        "KeyB",
        "KeyC",
        "KeyD",
        "KeyE",
        "KeyF",
        "KeyG",
        "KeyH",
        "KeyI",
        "KeyJ",
        "KeyK",
        "KeyL",
        "KeyM",
        "KeyN",
        "KeyO",
        "KeyP",
        "KeyQ",
        "KeyR",
        "KeyS",
        "KeyT",
        "KeyU",
        "KeyV",
        "KeyW",
        "KeyX",
        "KeyY",
        "KeyZ",
        "Digit0",
        "Digit1",
        "Digit2",
        "Digit3",
        "Digit4",
        "Digit5",
        "Digit6",
        "Digit7",
        "Digit8",
        "Digit9",
        "Minus",
        "Equal",
        "BracketLeft",
        "BracketRight",
        "Backslash",
        "Semicolon",
        "Quote",
        "Backquote",
        "Comma",
        "Period",
        "Slash",
        "Space",
        "Enter",
        "Tab",
        "Backspace",
        "Escape",
        "F1",
        "F2",
        "F3",
        "F4",
        "F5",
        "F6",
        "F7",
        "F8",
        "F9",
        "F10",
        "F11",
        "F12",
        "F13",
        "F14",
        "F15",
        "F16",
        "F17",
        "F18",
        "F19",
        "F20",
        "F21",
        "F22",
        "F23",
        "F24",
        "Insert",
        "Delete",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "ArrowUp",
        "ArrowDown",
        "ArrowLeft",
        "ArrowRight",
        "PrintScreen",
        "ScrollLock",
        "Pause",
        "Numpad0",
        "Numpad1",
        "Numpad2",
        "Numpad3",
        "Numpad4",
        "Numpad5",
        "Numpad6",
        "Numpad7",
        "Numpad8",
        "Numpad9",
        "NumpadAdd",
        "NumpadSubtract",
        "NumpadMultiply",
        "NumpadDivide",
        "NumpadDecimal",
        "NumpadEnter",
        "MediaPlayPause",
        "MediaStop",
        "MediaTrackNext",
        "MediaTrackPrevious",
        "AudioVolumeMute",
        "AudioVolumeDown",
        "AudioVolumeUp",
    ];

    /// Wraps a string, unchecked.
    pub fn new(chord: impl Into<String>) -> Self {
        Self(chord.into())
    }

    /// The chord as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Why this is not a valid chord, or `None` if it is. The engine rejects a chord
    /// with this reason, so every implementation gives the same one.
    pub fn problem(&self) -> Option<String> {
        if self.0.is_empty() {
            return Some("must not be empty".into());
        }
        if self.0.split('+').any(str::is_empty) {
            return Some("must be modifiers and a key joined by single `+` signs".into());
        }
        let mut parts: Vec<&str> = self.0.split('+').collect();
        let key = parts.pop().unwrap_or_default();
        if !Self::KEYS.contains(&key) {
            return Some(if Self::MODIFIERS.contains(&key) {
                "must end in a key, not a modifier".into()
            } else {
                format!("`{key}` is not a key code this engine knows, such as `KeyA` or `F1`")
            });
        }
        let mut next = 0;
        for part in parts {
            match Self::MODIFIERS.iter().position(|m| *m == part) {
                Some(at) if at >= next => next = at + 1,
                Some(_) => {
                    return Some(
                        "modifiers must each appear once, in the order Ctrl, Alt, Shift, Meta"
                            .into(),
                    );
                }
                None => {
                    return Some(format!(
                        "`{part}` is not a modifier: use Ctrl, Alt, Shift or Meta"
                    ));
                }
            }
        }
        None
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Chord {
    fn from(chord: &str) -> Self {
        Self::new(chord)
    }
}

/// One hotkey binding, as `state.get`, `delete.plan` and a batch result report it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HotkeyBinding {
    /// The chord.
    pub chord: Chord,
    /// The action request the chord runs, as `hotkey.set` bound it. A client sends it as
    /// an `action` call when the chord is pressed. Read as plain JSON, so a client older
    /// than an action type still reads the collection: it keeps the binding and can't
    /// run it. [`HotkeyBinding::action`] parses it.
    #[schemars(with = "Action")]
    pub action: serde_json::Value,
}

impl HotkeyBinding {
    /// A binding of `chord` to `action`.
    pub fn new(chord: Chord, action: &Action) -> Self {
        HotkeyBinding {
            chord,
            // An action is plain data: IDs, strings and a secret string, which always
            // serialize. A stream key is never bound, so none is copied here.
            action: serde_json::to_value(action).expect("invariant: an Action serializes to JSON"),
        }
    }

    /// The bound action, or `None` when it is one this version doesn't know.
    pub fn action(&self) -> Option<Action> {
        serde_json::from_value(self.action.clone()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{Chord, HotkeyBinding};
    use crate::command::Action;
    use crate::id::SceneId;
    use serde_json::json;

    #[test]
    fn valid_chords_have_no_problem() {
        for chord in [
            "F1",
            "Pause",
            "Ctrl+KeyA",
            "Ctrl+Alt+Shift+Meta+Digit1",
            "Shift+Space",
        ] {
            assert_eq!(Chord::new(chord).problem(), None, "{chord}");
        }
    }

    #[test]
    fn invalid_chords_say_why() {
        let cases = [
            ("", "must not be empty"),
            (
                "Ctrl+",
                "must be modifiers and a key joined by single `+` signs",
            ),
            (
                "Ctrl++KeyA",
                "must be modifiers and a key joined by single `+` signs",
            ),
            (
                "Ctrl+a",
                "`a` is not a key code this engine knows, such as `KeyA` or `F1`",
            ),
            ("Ctrl", "must end in a key, not a modifier"),
            (
                "Ctrl+ShiftLeft",
                "`ShiftLeft` is not a key code this engine knows, such as `KeyA` or `F1`",
            ),
            (
                "Shift+Ctrl+KeyA",
                "modifiers must each appear once, in the order Ctrl, Alt, Shift, Meta",
            ),
            (
                "Ctrl+Ctrl+KeyA",
                "modifiers must each appear once, in the order Ctrl, Alt, Shift, Meta",
            ),
            (
                "Control+KeyA",
                "`Control` is not a modifier: use Ctrl, Alt, Shift or Meta",
            ),
            (
                "Ctrl+ KeyA",
                "` KeyA` is not a key code this engine knows, such as `KeyA` or `F1`",
            ),
            (
                "Ctrl +KeyA",
                "`Ctrl ` is not a modifier: use Ctrl, Alt, Shift or Meta",
            ),
        ];
        for (chord, reason) in cases {
            assert_eq!(
                Chord::new(chord).problem().as_deref(),
                Some(reason),
                "{chord:?}"
            );
        }
    }

    #[test]
    fn a_binding_reads_back_its_action_and_tolerates_unknown_ones() {
        let switch = Action::SceneSwitch {
            scene_id: SceneId::new("intro"),
            preset_id: None,
        };
        let binding = HotkeyBinding::new(Chord::new("F1"), &switch);
        assert_eq!(
            serde_json::to_value(&binding).expect("serializes"),
            json!({"chord": "F1", "action": {"type": "scene.switch", "scene_id": "intro"}})
        );
        assert_eq!(binding.action(), Some(switch));
        let later: HotkeyBinding = serde_json::from_value(
            json!({"chord": "F2", "action": {"type": "studio.later", "x": 1}}),
        )
        .expect("an unknown action still parses");
        assert_eq!(later.action(), None);
    }
}
