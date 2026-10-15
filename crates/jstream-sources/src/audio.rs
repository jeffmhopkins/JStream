//! The audio-only kinds: `audio_input`, `desktop_audio` and `app_audio`. They make no
//! frames; the engine records each through the platform's audio I/O into a mixer input,
//! from the [`AudioTarget`] its settings name.

use crate::describe::{kind, param};
use crate::{Context, Source, SourceError, SourceKind};
use jstream_protocol::{KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::Value;

/// The kind that records an input device, such as a microphone.
pub(crate) const AUDIO_INPUT: &str = "audio_input";
/// The kind that records what the default output device plays.
pub(crate) const DESKTOP_AUDIO: &str = "desktop_audio";
/// The kind that records what one application plays.
pub(crate) const APP_AUDIO: &str = "app_audio";

/// What an audio-only source records.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AudioTarget {
    /// One input device by its ID, or with `None` the system's default input, followed
    /// when the default changes.
    Input {
        /// The device's ID, as the platform lists it.
        device: Option<String>,
    },
    /// What the system's default output device plays, followed when the default changes.
    Desktop,
    /// What one application plays: the running process whose executable has this file
    /// name, matched without regard to case, found again when the application restarts.
    App {
        /// The executable's file name, such as `game.exe`. Empty matches nothing.
        exe: String,
    },
}

impl AudioTarget {
    /// What a source of `kind_id` with these settings records, or `None` when `kind_id`
    /// is not an audio-only kind. Settings are complete, as the graph stores them; a
    /// missing or mistyped one reads as its default.
    pub fn of(kind_id: &str, settings: &Settings) -> Option<Self> {
        let text = |id: &str| {
            settings
                .get(id)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        match kind_id {
            AUDIO_INPUT => {
                let device = text("device");
                Some(Self::Input {
                    device: (!device.is_empty()).then_some(device),
                })
            }
            DESKTOP_AUDIO => Some(Self::Desktop),
            APP_AUDIO => Some(Self::App { exe: text("exe") }),
            _ => None,
        }
    }
}

/// An audio-only kind: described, so its sources can be created and configured, and
/// recorded by the engine rather than created here.
pub(crate) struct AudioKind(KindDescriptor);

impl SourceKind for AudioKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, _: &Settings, _: &Context) -> Result<Box<dyn Source>, SourceError> {
        Err(SourceError::AudioOnly {
            kind_id: self.0.kind_id.clone(),
        })
    }
}

/// A one-line text setting.
fn line(max_len: u32) -> ParamType {
    ParamType::String {
        multiline: false,
        max_len,
    }
}

/// The audio-only kinds.
pub(crate) fn kinds() -> Vec<AudioKind> {
    vec![
        AudioKind(KindDescriptor {
            params: vec![ParamDescriptor {
                description: Some(
                    "The input device's ID. Empty follows the system's default input.".into(),
                ),
                ..param("device", "Device", line(512), Value::from(""))
            }],
            ..kind(AUDIO_INPUT, "Audio input", false, true)
        }),
        AudioKind(kind(DESKTOP_AUDIO, "Desktop audio", false, true)),
        AudioKind(KindDescriptor {
            params: vec![ParamDescriptor {
                description: Some(
                    "The application's executable file name, such as game.exe. It is \
                     found again when the application restarts."
                        .into(),
                ),
                ..param("exe", "Application", line(260), Value::from(""))
            }],
            ..kind(APP_AUDIO, "Application audio", false, true)
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pairs: &[(&str, &str)]) -> Settings {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Value::from(*v)))
            .collect()
    }

    #[test]
    fn targets_follow_the_kind_and_its_settings() {
        assert_eq!(
            AudioTarget::of(AUDIO_INPUT, &settings(&[("device", "")])),
            Some(AudioTarget::Input { device: None })
        );
        assert_eq!(
            AudioTarget::of(AUDIO_INPUT, &settings(&[("device", " mic-1 ")])),
            Some(AudioTarget::Input {
                device: Some("mic-1".into())
            })
        );
        assert_eq!(
            AudioTarget::of(DESKTOP_AUDIO, &Settings::new()),
            Some(AudioTarget::Desktop)
        );
        assert_eq!(
            AudioTarget::of(APP_AUDIO, &settings(&[("exe", "Game.exe")])),
            Some(AudioTarget::App {
                exe: "Game.exe".into()
            })
        );
        assert_eq!(
            AudioTarget::of(APP_AUDIO, &Settings::new()),
            Some(AudioTarget::App { exe: String::new() })
        );
        assert_eq!(AudioTarget::of("image", &Settings::new()), None);
    }
}
