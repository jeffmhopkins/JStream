//! The audio-only kinds: `audio_input`, `desktop_audio`, `app_audio` and
//! `network_audio`. They make no frames; the engine records each into a mixer input,
//! from the [`AudioTarget`] its settings name: a device or application through the
//! platform's audio I/O, a network sender through the engine's receiver (ADR-0122).

use crate::describe::{kind, param};
use crate::{Context, Source, SourceError, SourceKind};
use jstream_protocol::{EnumOption, KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::Value;

/// The kind that records an input device, such as a microphone.
pub(crate) const AUDIO_INPUT: &str = "audio_input";
/// The kind that records what the default output device plays.
pub(crate) const DESKTOP_AUDIO: &str = "desktop_audio";
/// The kind that records what one application plays.
pub(crate) const APP_AUDIO: &str = "app_audio";
/// The kind that receives a network sender's audio, such as a DAW's over JTunnel.
pub(crate) const NETWORK_AUDIO: &str = "network_audio";

/// The longest network sender name: JTunnel's tunnel names are at most 31 bytes.
pub const MAX_SENDER_NAME: usize = 31;
/// The shortest and longest lead a network audio source holds its frames for, in
/// milliseconds, and the default.
pub const LATENCY_MS: (u32, u32, u32) = (5, 500, 40);

/// How a network audio source receives its sender.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Transport {
    /// JTunnel's UDP packets (wire version 2), subscribed to by tunnel name.
    Jtunnel,
}

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
    /// A network sender, by its name on the transport. Empty, or a name the engine has
    /// not accepted, records nothing.
    Network {
        /// How it is received.
        transport: Transport,
        /// The sender's name: a JTunnel tunnel name.
        sender: String,
        /// How long its frames are held past their jitter-free arrival, so a late one
        /// still plays, in milliseconds.
        latency_ms: u32,
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
            NETWORK_AUDIO => {
                let (min, max, default) = LATENCY_MS;
                let latency_ms = settings
                    .get("latency_ms")
                    .and_then(Value::as_u64)
                    .and_then(|ms| u32::try_from(ms).ok())
                    .filter(|ms| (min..=max).contains(ms))
                    .unwrap_or(default);
                Some(Self::Network {
                    transport: Transport::Jtunnel,
                    sender: text("sender"),
                    latency_ms,
                })
            }
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
        AudioKind(KindDescriptor {
            params: vec![
                ParamDescriptor {
                    description: Some(
                        "How the audio arrives. JTunnel is a DAW plugin that sends a \
                         track over the local network."
                            .into(),
                    ),
                    ..param(
                        "transport",
                        "Transport",
                        ParamType::Enum {
                            options: vec![EnumOption {
                                value: "jtunnel".into(),
                                label: "JTunnel".into(),
                            }],
                        },
                        Value::from("jtunnel"),
                    )
                },
                ParamDescriptor {
                    description: Some(
                        "The sender's name, as its plugin shows it. JStream receives only \
                         from senders accepted on this machine."
                            .into(),
                    ),
                    ..param(
                        "sender",
                        "Sender",
                        line(MAX_SENDER_NAME as u32),
                        Value::from(""),
                    )
                },
                ParamDescriptor {
                    description: Some(
                        "How long frames wait before they play, so one delayed on the \
                         network still arrives in time. Higher survives a busier network; \
                         lower is closer to live."
                            .into(),
                    ),
                    ..param(
                        "latency_ms",
                        "Latency",
                        ParamType::Int {
                            min: Some(i64::from(LATENCY_MS.0)),
                            max: Some(i64::from(LATENCY_MS.1)),
                            step: Some(1),
                            unit: Some("ms".into()),
                        },
                        Value::from(LATENCY_MS.2),
                    )
                },
            ],
            ..kind(NETWORK_AUDIO, "Network audio", false, true)
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
        let mut network = settings(&[("transport", "jtunnel"), ("sender", " Bitwig 2 ")]);
        network.insert("latency_ms", Value::from(25));
        assert_eq!(
            AudioTarget::of(NETWORK_AUDIO, &network),
            Some(AudioTarget::Network {
                transport: Transport::Jtunnel,
                sender: "Bitwig 2".into(),
                latency_ms: 25,
            })
        );
        // A latency out of range, or missing, reads as the default.
        network.insert("latency_ms", Value::from(9000));
        assert!(matches!(
            AudioTarget::of(NETWORK_AUDIO, &network),
            Some(AudioTarget::Network { latency_ms: 40, .. })
        ));
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
