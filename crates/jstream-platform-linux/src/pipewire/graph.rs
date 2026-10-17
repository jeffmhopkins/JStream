//! What the PipeWire registry says about the audio graph, and what it means for
//! `AudioIo` (ADR-0060 §4): the devices, the applications playing audio, and which node
//! a stream for each target connects to. Platform-independent, so it is tested
//! everywhere; the registry itself is read in `audio`.
//!
//! - A **device** is a node whose `media.class` is `Audio/Source`,
//!   `Audio/Source/Virtual` (input), `Audio/Sink` (output) or `Audio/Duplex` (both). Its
//!   [`DeviceId`] is its `node.name`, which is stable across restarts, unlike its global
//!   id. Its name is `node.description`, else `node.nick`, else `node.name`.
//! - The **defaults** are the `default.audio.source` and `default.audio.sink` keys of the
//!   `default` metadata object, each `{"name": "<node.name>"}`.
//! - An **application playing audio** is a `Stream/Output/Audio` node. Its process is
//!   the node's `application.process.id` and `application.process.binary` when it sets
//!   them, else its client's (by the node's `client.id`): the client's own
//!   `application.process.id`, else `pipewire.sec.pid`, which the daemon fills in from
//!   the socket.

use std::collections::BTreeMap;

use jstream_platform::media::{AudioFormat, ChannelLayout, SampleFormat};
use jstream_platform::{
    AudioDeviceInfo, AudioDirection, AudioInputTarget, AudioOutputTarget, AudioProcess, DeviceId,
    PlatformError,
};

use crate::spa::audio::layout_for;

/// PipeWire's usual graph rate, when the core doesn't report `default.clock.rate`.
pub(crate) const DEFAULT_RATE: u32 = 48_000;

/// One node from the registry, with the properties this crate reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    /// Its global id on this connection.
    pub(crate) id: u32,
    pub(crate) name: String,
    pub(crate) class: String,
    description: Option<String>,
    nick: Option<String>,
    /// `object.serial`: unique for the daemon's lifetime, unlike `id`, which is reused.
    serial: Option<String>,
    pid: Option<u32>,
    binary: Option<String>,
    app_name: Option<String>,
    /// `client.id`: the global id of the client that made it.
    client: Option<u32>,
    channels: Option<u32>,
}

/// One client from the registry: who a stream node's process is when the node doesn't
/// say.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Client {
    pid: Option<u32>,
    binary: Option<String>,
    app_name: Option<String>,
}

/// The property `key`, when it is there and not empty.
fn prop(props: &[(String, String)], key: &str) -> Option<String> {
    props
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty())
}

fn number(props: &[(String, String)], key: &str) -> Option<u32> {
    prop(props, key).and_then(|v| v.trim().parse().ok())
}

impl Node {
    /// A node from its global's properties. `None` without a `node.name` or
    /// `media.class`: nothing this crate lists or connects to.
    pub(crate) fn from_props(id: u32, props: &[(String, String)]) -> Option<Self> {
        let get = |key: &str| prop(props, key);
        Some(Self {
            id,
            name: get("node.name")?,
            class: get("media.class")?,
            description: get("node.description"),
            nick: get("node.nick"),
            serial: get("object.serial"),
            pid: number(props, "application.process.id"),
            binary: get("application.process.binary"),
            app_name: get("application.name"),
            client: number(props, "client.id"),
            channels: number(props, "audio.channels"),
        })
    }

    fn records(&self) -> bool {
        matches!(
            self.class.as_str(),
            "Audio/Source" | "Audio/Source/Virtual" | "Audio/Duplex"
        )
    }

    fn plays(&self) -> bool {
        matches!(self.class.as_str(), "Audio/Sink" | "Audio/Duplex")
    }

    fn is_app_playback(&self) -> bool {
        self.class == "Stream/Output/Audio"
    }

    fn display_name(&self) -> String {
        self.description
            .clone()
            .or_else(|| self.nick.clone())
            .unwrap_or_else(|| self.name.clone())
    }

    fn layout(&self) -> ChannelLayout {
        layout_for(self.channels)
    }
}

/// The `name` of a default-device metadata value, `{"name":"alsa_output.pci-…"}`. Only
/// that one string field is read, with JSON's escapes; `None` for anything else.
pub(crate) fn default_name(json: &str) -> Option<String> {
    let rest = json.trim().strip_prefix('{')?;
    let at = rest.find("\"name\"")?;
    let rest = rest[at + "\"name\"".len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let mut chars = rest.strip_prefix('"')?.chars();
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => break,
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                c => out.push(c),
            },
            c => out.push(c),
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The audio graph as one registry read saw it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Graph {
    /// By global id.
    nodes: BTreeMap<u32, Node>,
    /// By global id.
    clients: BTreeMap<u32, Client>,
    default_source: Option<String>,
    default_sink: Option<String>,
    /// The core's `default.clock.rate`.
    clock_rate: Option<u32>,
}

/// How a stream connects: which node it targets (by the `target.object` property), whether
/// it records a sink's monitor, which node's removal ends it, and its format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    /// `target.object`: a node name or serial. `None` follows the default.
    pub(crate) target: Option<String>,
    /// `stream.capture.sink`: record what a sink plays.
    pub(crate) capture_sink: bool,
    /// The node whose removal is the device being lost; `None` for a default, which
    /// PipeWire's session manager moves the stream off by itself.
    pub(crate) watch: Option<u32>,
    pub(crate) rate: u32,
    pub(crate) layout: ChannelLayout,
    /// What the stream is on, for errors.
    pub(crate) what: String,
}

impl Plan {
    /// The handler's format: interleaved `f32` at the plan's rate and layout.
    pub(crate) fn format(&self) -> Result<AudioFormat, PlatformError> {
        AudioFormat::new(self.rate, self.layout, SampleFormat::F32Interleaved).map_err(|e| {
            PlatformError::Unsupported {
                what: format!("an audio format: {e}"),
            }
        })
    }
}

fn not_found(kind: &'static str, id: impl Into<String>) -> PlatformError {
    PlatformError::NotFound {
        kind,
        id: id.into(),
    }
}

impl Graph {
    /// Adds or replaces a node from a `global` event; a global that isn't a node this
    /// crate reads is ignored.
    pub(crate) fn add(&mut self, id: u32, props: &[(String, String)]) {
        if let Some(node) = Node::from_props(id, props) {
            self.nodes.insert(id, node);
        }
    }

    /// Adds or replaces a client from a `global` event.
    pub(crate) fn add_client(&mut self, id: u32, props: &[(String, String)]) {
        self.clients.insert(
            id,
            Client {
                pid: number(props, "application.process.id")
                    .or_else(|| number(props, "pipewire.sec.pid")),
                binary: prop(props, "application.process.binary"),
                app_name: prop(props, "application.name"),
            },
        );
    }

    /// Forgets a node or client, from a `global_remove` event.
    pub(crate) fn remove(&mut self, id: u32) {
        self.nodes.remove(&id);
        self.clients.remove(&id);
    }

    /// The process playing `n`, and its executable: the node's own, else its client's.
    /// `None` when neither says.
    fn process_of(&self, n: &Node) -> Option<(u32, String)> {
        let client = n.client.and_then(|c| self.clients.get(&c));
        let pid = n.pid.or_else(|| client?.pid)?;
        let exe = n
            .binary
            .clone()
            .or_else(|| client?.binary.clone())
            .or_else(|| n.app_name.clone())
            .or_else(|| client?.app_name.clone())
            .unwrap_or_else(|| n.name.clone());
        Some((pid, exe))
    }

    /// Takes a `default` metadata property. `None` clears it.
    pub(crate) fn set_default(&mut self, key: &str, value: Option<&str>) {
        let name = value.and_then(default_name);
        match key {
            "default.audio.source" => self.default_source = name,
            "default.audio.sink" => self.default_sink = name,
            _ => {}
        }
    }

    /// Takes the core's properties, for its clock rate.
    pub(crate) fn set_core_props(&mut self, props: &[(String, String)]) {
        if let Some((_, v)) = props.iter().find(|(k, _)| k == "default.clock.rate") {
            self.clock_rate = v.trim().parse().ok().filter(|&r| r > 0);
        }
    }

    fn rate(&self) -> u32 {
        self.clock_rate.unwrap_or(DEFAULT_RATE)
    }

    /// The input and output devices, inputs first, each in id order. A duplex node is
    /// listed once each way.
    pub(crate) fn devices(&self) -> Vec<AudioDeviceInfo> {
        let mut out = Vec::new();
        for (direction, keep, default) in [
            (
                AudioDirection::Input,
                Node::records as fn(&Node) -> bool,
                &self.default_source,
            ),
            (AudioDirection::Output, Node::plays, &self.default_sink),
        ] {
            out.extend(
                self.nodes
                    .values()
                    .filter(|n| keep(n))
                    .map(|n| AudioDeviceInfo {
                        id: DeviceId::new(n.name.clone()),
                        name: n.display_name(),
                        direction,
                        is_default: default.as_deref() == Some(n.name.as_str()),
                    }),
            );
        }
        out
    }

    /// The processes with a playback stream, once each, by pid. A stream whose process
    /// is unknown (see the module doc) isn't listed. PipeWire doesn't report a parent.
    pub(crate) fn processes(&self) -> Vec<AudioProcess> {
        let mut by_pid: BTreeMap<u32, AudioProcess> = BTreeMap::new();
        for n in self.nodes.values().filter(|n| n.is_app_playback()) {
            let Some((pid, exe)) = self.process_of(n) else {
                continue;
            };
            by_pid.entry(pid).or_insert(AudioProcess {
                pid,
                parent_pid: None,
                exe,
            });
        }
        by_pid.into_values().collect()
    }

    fn named(&self, name: &str, keep: fn(&Node) -> bool) -> Option<&Node> {
        self.nodes.values().find(|n| keep(n) && n.name == name)
    }

    fn default_node(&self, name: Option<&String>, keep: fn(&Node) -> bool) -> Option<&Node> {
        self.named(name?, keep)
    }

    /// How to record `target`.
    ///
    /// # Errors
    /// [`PlatformError::NotFound`] for a device or process with no node now.
    pub(crate) fn input(&self, target: &AudioInputTarget) -> Result<Plan, PlatformError> {
        let rate = self.rate();
        let plan = |node: Option<&Node>, target, capture_sink, watch: bool, what: String| Plan {
            target,
            capture_sink,
            watch: if watch { node.map(|n| n.id) } else { None },
            rate,
            layout: node.map_or(ChannelLayout::Stereo, Node::layout),
            what,
        };
        Ok(match target {
            AudioInputTarget::DefaultInput => {
                let node = self.default_node(self.default_source.as_ref(), Node::records);
                plan(node, None, false, false, "the default input".into())
            }
            AudioInputTarget::Input(id) => {
                let node = self
                    .named(id.as_str(), Node::records)
                    .ok_or_else(|| not_found("audio device", id.as_str()))?;
                plan(
                    Some(node),
                    Some(node.name.clone()),
                    false,
                    true,
                    node.display_name(),
                )
            }
            AudioInputTarget::DefaultOutputLoopback => {
                let node = self.default_node(self.default_sink.as_ref(), Node::plays);
                plan(node, None, true, false, "the default output's audio".into())
            }
            AudioInputTarget::OutputLoopback(id) => {
                let node = self
                    .named(id.as_str(), Node::plays)
                    .ok_or_else(|| not_found("audio device", id.as_str()))?;
                plan(
                    Some(node),
                    Some(node.name.clone()),
                    true,
                    true,
                    format!("{}'s audio", node.display_name()),
                )
            }
            AudioInputTarget::ProcessLoopback { pid } => {
                // The process's first playback stream. Its children's streams, which
                // Windows' process loopback includes, aren't followed: PipeWire doesn't
                // say which process started which.
                let node = self
                    .nodes
                    .values()
                    .find(|n| {
                        n.is_app_playback() && self.process_of(n).is_some_and(|(p, _)| p == *pid)
                    })
                    .ok_or_else(|| not_found("application playing audio", pid.to_string()))?;
                let target = node.serial.clone().unwrap_or_else(|| node.name.clone());
                plan(
                    Some(node),
                    Some(target),
                    false,
                    true,
                    format!("process {pid}'s audio"),
                )
            }
            // A target kind added later.
            _ => {
                return Err(PlatformError::Unsupported {
                    what: format!("recording {target:?} through PipeWire"),
                });
            }
        })
    }

    /// How to play on `target`.
    ///
    /// # Errors
    /// [`PlatformError::NotFound`] for a device with no node now.
    pub(crate) fn output(&self, target: &AudioOutputTarget) -> Result<Plan, PlatformError> {
        let rate = self.rate();
        Ok(match target {
            AudioOutputTarget::DefaultOutput => {
                let node = self.default_node(self.default_sink.as_ref(), Node::plays);
                Plan {
                    target: None,
                    capture_sink: false,
                    watch: None,
                    rate,
                    layout: node.map_or(ChannelLayout::Stereo, Node::layout),
                    what: "the default output".into(),
                }
            }
            AudioOutputTarget::Output(id) => {
                let node = self
                    .named(id.as_str(), Node::plays)
                    .ok_or_else(|| not_found("audio device", id.as_str()))?;
                Plan {
                    target: Some(node.name.clone()),
                    capture_sink: false,
                    watch: Some(node.id),
                    rate,
                    layout: node.layout(),
                    what: node.display_name(),
                }
            }
            _ => {
                return Err(PlatformError::Unsupported {
                    what: format!("playing on {target:?} through PipeWire"),
                });
            }
        })
    }
}

/// When a packet's first frame was recorded (`capture`) or will play, in 100 ns
/// ticks on the media clock, from PipeWire's time report: `now_ns` (`CLOCK_MONOTONIC`
/// nanoseconds), `delay` in `rate` units (`num / den` seconds each) and the packet's
/// `frames` at `sample_rate`. A capture's first frame came `delay` and the packet's own
/// length before `now`; a playback's first frame plays `delay` after it. `None` when the
/// numbers overflow or `rate` is degenerate.
pub(crate) fn packet_hns(
    now_ns: i64,
    delay: i64,
    rate: (u32, u32),
    frames: u64,
    sample_rate: u32,
    capture: bool,
) -> Option<i64> {
    let (num, den) = rate;
    if num == 0 || den == 0 || sample_rate == 0 {
        return None;
    }
    let delay_ns = i128::from(delay) * i128::from(num) * 1_000_000_000 / i128::from(den);
    let length_ns = i128::from(frames) * 1_000_000_000 / i128::from(sample_rate);
    let ns = if capture {
        i128::from(now_ns) - delay_ns.max(0) - length_ns
    } else {
        i128::from(now_ns) + delay_ns.max(0)
    };
    i64::try_from(ns / 100).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// A laptop's graph: a mic, speakers, a virtual source, a duplex headset, a
    /// browser playing two streams, a client stream without a pid, and other globals.
    fn graph() -> Graph {
        let mut g = Graph::default();
        g.add(
            40,
            &props(&[
                ("node.name", "alsa_input.pci-0000_00_1f.3.analog-stereo"),
                ("node.description", "Built-in Audio Analog Stereo"),
                ("media.class", "Audio/Source"),
                ("audio.channels", "2"),
            ]),
        );
        g.add(
            41,
            &props(&[
                (
                    "node.name",
                    "alsa_output.pci-0000_00_1f.3.analog-surround-51",
                ),
                ("node.description", "Built-in Audio Surround 5.1"),
                ("media.class", "Audio/Sink"),
                ("audio.channels", "6"),
            ]),
        );
        g.add(
            42,
            &props(&[
                ("node.name", "echo-cancel-source"),
                ("node.nick", "Echo cancel"),
                ("media.class", "Audio/Source/Virtual"),
                ("audio.channels", "1"),
            ]),
        );
        g.add(
            43,
            &props(&[
                ("node.name", "bluez_headset"),
                ("media.class", "Audio/Duplex"),
            ]),
        );
        g.add(
            60,
            &props(&[
                ("node.name", "Firefox"),
                ("media.class", "Stream/Output/Audio"),
                ("object.serial", "812"),
                ("application.process.id", "4242"),
                ("application.process.binary", "firefox"),
            ]),
        );
        g.add(
            61,
            &props(&[
                ("node.name", "Firefox"),
                ("media.class", "Stream/Output/Audio"),
                ("object.serial", "815"),
                ("application.process.id", "4242"),
                ("application.process.binary", "firefox"),
            ]),
        );
        g.add(
            62,
            &props(&[
                ("node.name", "pw-cat"),
                ("media.class", "Stream/Output/Audio"),
            ]),
        );
        // A stream that names only its client, whose pid the daemon filled in.
        g.add(
            64,
            &props(&[
                ("node.name", "speech-dispatcher"),
                ("media.class", "Stream/Output/Audio"),
                ("client.id", "90"),
            ]),
        );
        g.add_client(
            90,
            &props(&[
                ("pipewire.sec.pid", "9208"),
                ("application.name", "speech-dispatcher"),
                ("application.process.binary", "sd_dummy"),
            ]),
        );
        g.add(
            63,
            &props(&[
                ("node.name", "mpv"),
                ("media.class", "Stream/Output/Audio"),
                ("application.process.id", "77"),
                ("application.name", "mpv Media Player"),
            ]),
        );
        g.add(
            70,
            &props(&[("node.name", "v4l2_input"), ("media.class", "Video/Source")]),
        );
        g.add(71, &props(&[("port.name", "capture_FL")]));
        g.set_default(
            "default.audio.source",
            Some(r#"{"name":"alsa_input.pci-0000_00_1f.3.analog-stereo"}"#),
        );
        g.set_default(
            "default.audio.sink",
            Some(r#"{ "name": "alsa_output.pci-0000_00_1f.3.analog-surround-51" }"#),
        );
        g
    }

    #[test]
    fn devices_are_audio_nodes_with_their_defaults() {
        let d = graph().devices();
        let row = |i: usize| {
            (
                d[i].id.as_str(),
                d[i].name.as_str(),
                d[i].direction,
                d[i].is_default,
            )
        };
        assert_eq!(d.len(), 5, "{d:?}");
        assert_eq!(
            row(0),
            (
                "alsa_input.pci-0000_00_1f.3.analog-stereo",
                "Built-in Audio Analog Stereo",
                AudioDirection::Input,
                true
            )
        );
        assert_eq!(
            row(1),
            (
                "echo-cancel-source",
                "Echo cancel",
                AudioDirection::Input,
                false
            )
        );
        assert_eq!(
            row(2),
            (
                "bluez_headset",
                "bluez_headset",
                AudioDirection::Input,
                false
            )
        );
        assert_eq!(
            row(3),
            (
                "alsa_output.pci-0000_00_1f.3.analog-surround-51",
                "Built-in Audio Surround 5.1",
                AudioDirection::Output,
                true
            )
        );
        assert_eq!(
            row(4),
            (
                "bluez_headset",
                "bluez_headset",
                AudioDirection::Output,
                false
            )
        );
    }

    #[test]
    fn processes_are_playback_streams_once_per_pid() {
        let p = graph().processes();
        assert_eq!(
            p,
            [
                AudioProcess {
                    pid: 77,
                    parent_pid: None,
                    exe: "mpv Media Player".into()
                },
                AudioProcess {
                    pid: 4242,
                    parent_pid: None,
                    exe: "firefox".into()
                },
                AudioProcess {
                    pid: 9208,
                    parent_pid: None,
                    exe: "sd_dummy".into()
                },
            ]
        );
    }

    #[test]
    fn a_stream_without_a_pid_is_its_clients_process() {
        let mut g = graph();
        let plan = g
            .input(&AudioInputTarget::ProcessLoopback { pid: 9208 })
            .unwrap();
        assert_eq!(
            (plan.target, plan.watch),
            (Some("speech-dispatcher".into()), Some(64))
        );
        // The client's own `application.process.id` wins over the socket's pid.
        g.add_client(
            90,
            &props(&[
                ("application.process.id", "9300"),
                ("pipewire.sec.pid", "9208"),
            ]),
        );
        assert!(
            g.processes()
                .iter()
                .any(|p| p.pid == 9300 && p.exe == "speech-dispatcher")
        );
        // Without the client, the stream has no process.
        g.remove(90);
        assert!(g.processes().iter().all(|p| p.pid != 9300 && p.pid != 9208));
    }

    #[test]
    fn inputs_plan_their_target_format_and_watch() {
        let mut g = graph();
        g.set_core_props(&props(&[("default.clock.rate", "44100")]));

        let default = g.input(&AudioInputTarget::DefaultInput).unwrap();
        assert_eq!(
            (default.target, default.capture_sink, default.watch),
            (None, false, None)
        );
        assert_eq!(
            (default.rate, default.layout),
            (44_100, ChannelLayout::Stereo)
        );

        let mic = g
            .input(&AudioInputTarget::Input(DeviceId::new(
                "echo-cancel-source",
            )))
            .unwrap();
        assert_eq!(mic.target.as_deref(), Some("echo-cancel-source"));
        assert_eq!((mic.watch, mic.layout), (Some(42), ChannelLayout::Mono));

        let desktop = g.input(&AudioInputTarget::DefaultOutputLoopback).unwrap();
        assert_eq!((desktop.target, desktop.capture_sink), (None, true));
        assert_eq!(desktop.layout, ChannelLayout::Surround51);

        let speakers = g
            .input(&AudioInputTarget::OutputLoopback(DeviceId::new(
                "alsa_output.pci-0000_00_1f.3.analog-surround-51",
            )))
            .unwrap();
        assert!(speakers.capture_sink);
        assert_eq!(speakers.watch, Some(41));

        // The first of the process's streams, by its serial.
        let app = g
            .input(&AudioInputTarget::ProcessLoopback { pid: 4242 })
            .unwrap();
        assert_eq!(
            (app.target.as_deref(), app.capture_sink, app.watch),
            (Some("812"), false, Some(60))
        );
        // Without a serial, its name.
        let mpv = g
            .input(&AudioInputTarget::ProcessLoopback { pid: 77 })
            .unwrap();
        assert_eq!(mpv.target.as_deref(), Some("mpv"));
        assert_eq!(mpv.format().unwrap().sample_rate(), 44_100);
    }

    #[test]
    fn missing_targets_are_not_found() {
        let g = graph();
        let not_found =
            |r: Result<Plan, PlatformError>| matches!(r, Err(PlatformError::NotFound { .. }));
        assert!(not_found(
            g.input(&AudioInputTarget::Input(DeviceId::new("gone")))
        ));
        // A sink isn't an input, and a source isn't an output.
        assert!(not_found(g.input(&AudioInputTarget::Input(DeviceId::new(
            "alsa_output.pci-0000_00_1f.3.analog-surround-51"
        )))));
        assert!(not_found(g.output(&AudioOutputTarget::Output(
            DeviceId::new("alsa_input.pci-0000_00_1f.3.analog-stereo")
        ))));
        assert!(not_found(g.input(&AudioInputTarget::OutputLoopback(
            DeviceId::new("gone")
        ))));
        assert!(not_found(
            g.input(&AudioInputTarget::ProcessLoopback { pid: 1 })
        ));
    }

    #[test]
    fn outputs_plan_their_target() {
        let g = graph();
        let default = g.output(&AudioOutputTarget::DefaultOutput).unwrap();
        assert_eq!((default.target, default.watch), (None, None));
        assert_eq!(
            (default.rate, default.layout),
            (DEFAULT_RATE, ChannelLayout::Surround51)
        );
        let headset = g
            .output(&AudioOutputTarget::Output(DeviceId::new("bluez_headset")))
            .unwrap();
        assert_eq!(
            (headset.target.as_deref(), headset.watch),
            (Some("bluez_headset"), Some(43))
        );
    }

    #[test]
    fn a_removed_node_and_a_cleared_default_are_forgotten() {
        let mut g = graph();
        g.remove(42);
        g.set_default("default.audio.source", None);
        let d = g.devices();
        assert!(d.iter().all(|d| d.id.as_str() != "echo-cancel-source"));
        assert!(
            d.iter()
                .filter(|d| d.direction == AudioDirection::Input)
                .all(|d| !d.is_default)
        );
        // An unrelated key and a bad clock rate are ignored.
        g.set_default("default.video.source", Some(r#"{"name":"x"}"#));
        g.set_core_props(&props(&[("default.clock.rate", "fast")]));
        assert_eq!(
            g.output(&AudioOutputTarget::DefaultOutput).unwrap().rate,
            DEFAULT_RATE
        );
    }

    #[test]
    fn packet_times_account_for_delay_and_length() {
        // 1 s on the clock, a 480-sample delay at 48 kHz (10 ms), a 480-frame packet.
        let now = 1_000_000_000;
        let rate = (1, 48_000);
        assert_eq!(
            packet_hns(now, 480, rate, 480, 48_000, true),
            Some((1_000_000_000 - 10_000_000 - 10_000_000) / 100)
        );
        assert_eq!(
            packet_hns(now, 480, rate, 480, 48_000, false),
            Some((1_000_000_000 + 10_000_000) / 100)
        );
        // A negative delay counts as none.
        assert_eq!(packet_hns(now, -5, rate, 0, 48_000, true), Some(now / 100));
        assert_eq!(packet_hns(now, 0, (0, 1), 0, 48_000, true), None);
        assert_eq!(packet_hns(now, 0, rate, 0, 0, true), None);
        assert_eq!(
            packet_hns(i64::MAX, i64::MAX, (u32::MAX, 1), 0, 1, false),
            None
        );
    }

    #[test]
    fn default_names_parse_from_json() {
        assert_eq!(default_name(r#"{"name":"a"}"#).as_deref(), Some("a"));
        assert_eq!(
            default_name(r#"  { "name" : "alsa_output.x" , "other": 1 }"#).as_deref(),
            Some("alsa_output.x")
        );
        assert_eq!(
            default_name(r#"{"name":"q\"uote\\dé"}"#).as_deref(),
            Some("q\"uote\\dé")
        );
        for bad in [
            "",
            "name",
            "{}",
            r#"{"name":1}"#,
            r#"{"name":""}"#,
            r#"{"name":"open"#,
        ] {
            assert_eq!(default_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn nodes_need_a_name_and_a_class() {
        assert!(Node::from_props(1, &props(&[("node.name", "x")])).is_none());
        assert!(Node::from_props(1, &props(&[("media.class", "Audio/Sink")])).is_none());
        assert!(
            Node::from_props(
                1,
                &props(&[("node.name", ""), ("media.class", "Audio/Sink")])
            )
            .is_none()
        );
        let n = Node::from_props(
            1,
            &props(&[
                ("node.name", "x"),
                ("media.class", "Audio/Sink"),
                ("audio.channels", "two"),
                ("application.process.id", "-3"),
            ]),
        )
        .unwrap();
        assert_eq!((n.channels, n.pid), (None, None));
    }
}
