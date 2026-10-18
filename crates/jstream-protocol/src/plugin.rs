//! WASM automation plugins on the wire (ADR-0069): the capability classes a person
//! grants a plugin, the grant itself, a plugin's state, and the table that places every
//! protocol command in one class or under "never".
//!
//! A plugin calls the API through a scoped `Control` that checks each call against
//! [`plugin_classes`] before it reaches the engine, and a call outside the plugin's grant
//! fails with `forbidden`. The table lives here, beside the commands, so a new command
//! can't ship unplaced: `every_command_has_a_plugin_scope` fails until a PR places it, and
//! until then it is "never".

use crate::command::{Action, Edit};
use crate::envelope::Call;
use crate::error::ProtocolError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// A capability class: a set of commands a person grants a plugin as one (ADR-0069 S2).
/// `read` is always granted.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PluginClass {
    /// Every query but the never-granted ones, `kind.*` and `delete.plan`. Always
    /// granted.
    Read,
    /// The scene, item, source, filter, bus, send, hotkey and canvas edits, live
    /// parameter edits (`live.*`), and undo and redo on the plugin's own session.
    Edit,
    /// What changes what is on air: `scene.switch`.
    Live,
    /// `output.start` and `output.stop` on outputs that already exist.
    Outputs,
}

impl fmt::Display for PluginClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PluginClass::Read => "read",
            PluginClass::Edit => "edit",
            PluginClass::Live => "live",
            PluginClass::Outputs => "outputs",
        })
    }
}

/// Where one command sits for plugins: in a class, or never granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginScope {
    /// A plugin granted this class may call it.
    Class(PluginClass),
    /// No plugin may call it, whatever it is granted.
    Never,
}

use PluginClass::{Edit as E, Live as L, Outputs as O, Read as R};
use PluginScope::{Class, Never as N};

/// Every protocol command a client can send, by name, and where it sits for plugins
/// (ADR-0069 S2). A name missing from this table is [`PluginScope::Never`], and the
/// test `every_command_has_a_plugin_scope` fails until a PR places it.
pub const PLUGIN_SCOPES: &[(&str, PluginScope)] = &[
    // Queries.
    ("scene.list", Class(R)),
    ("output.status", Class(R)),
    ("source.list", Class(R)),
    ("item.list", Class(R)),
    ("state.get", Class(R)),
    ("kind.list", Class(R)),
    ("kind.describe", Class(R)),
    ("output.list", Class(R)),
    ("studio.get", Class(R)),
    ("encoder.list", Class(R)),
    ("video_mix.list", Class(R)),
    ("live.list", Class(R)),
    ("bus.list", Class(R)),
    ("delete.plan", Class(R)),
    ("capture.targets", Class(R)),
    // The pixels of a user's capture could leak through any granted host; with no
    // filesystem grant there is nowhere safe for them.
    ("video.snapshot", N),
    ("youtube.status", N),
    ("youtube.list_broadcasts", N),
    ("youtube.list_streams", N),
    ("device.list", N),
    // Edits.
    ("scene.create", Class(E)),
    ("scene.rename", Class(E)),
    ("scene.set_role", Class(E)),
    ("scene.delete", Class(E)),
    ("scene.reorder", Class(E)),
    ("item.add", Class(E)),
    ("item.remove", Class(E)),
    ("item.move", Class(E)),
    ("item.set_transform", Class(E)),
    ("item.set_geometry", Class(E)),
    ("item.set_visible", Class(E)),
    ("item.reorder", Class(E)),
    ("item.set_crop", Class(E)),
    ("item.set_opacity", Class(E)),
    ("item.set_blend", Class(E)),
    ("item.set_source", Class(E)),
    ("source.create", Class(E)),
    ("source.rename", Class(E)),
    ("source.set_settings", Class(E)),
    ("source.duplicate", Class(E)),
    ("source.delete", Class(E)),
    ("source.set_audio", Class(E)),
    ("filter.add", Class(E)),
    ("filter.remove", Class(E)),
    ("filter.set_enabled", Class(E)),
    ("filter.set_settings", Class(E)),
    ("filter.reorder", Class(E)),
    ("bus.create", Class(E)),
    ("bus.rename", Class(E)),
    ("bus.set_fader", Class(E)),
    ("bus.delete", Class(E)),
    ("send.set", Class(E)),
    ("send.remove", Class(E)),
    ("canvas.set_size", Class(E)),
    ("hotkey.set", Class(E)),
    ("hotkey.clear", Class(E)),
    ("output.create", N),
    ("output.set_target", N),
    ("output.delete", N),
    ("output.set_encoder", N),
    ("encoder.create", N),
    ("encoder.update", N),
    ("encoder.delete", N),
    // Not named in any class by ADR-0069, so "never" until a PR places them: a video
    // mix decides what an output shows.
    ("video_mix.create", N),
    ("video_mix.update", N),
    ("video_mix.delete", N),
    ("output.set_video_mix", N),
    // Actions.
    ("live.set", Class(E)),
    ("live.cancel", Class(E)),
    ("scene.switch", Class(L)),
    ("output.start", Class(O)),
    ("output.stop", Class(O)),
    ("output.set_key", N),
    ("output.clear_key", N),
    ("youtube.sign_in", N),
    ("youtube.sign_out", N),
    ("youtube.create_broadcast", N),
    ("youtube.bind_broadcast", N),
    ("youtube.transition_broadcast", N),
    ("pairing.start", N),
    ("device.revoke", N),
    // Needs a filesystem grant, like `video.snapshot`.
    ("preview.subscribe", N),
    // Not named in any class, so "never" until a PR places them.
    ("preview.unsubscribe", N),
    ("audio.reset_loudness", N),
    // A person installs, grants and re-enables plugins; a plugin never does (G1).
    ("plugin.install", N),
    ("plugin.grant", N),
    ("plugin.enable", N),
    // History, on the plugin's own session.
    ("history.undo", Class(E)),
    ("history.redo", Class(E)),
];

/// Where the command `name` sits for plugins. A name the table doesn't place is
/// [`PluginScope::Never`].
pub fn plugin_scope(name: &str) -> PluginScope {
    PLUGIN_SCOPES
        .iter()
        .find(|(placed, _)| *placed == name)
        .map_or(PluginScope::Never, |(_, scope)| *scope)
}

/// Why a call can't be made by any plugin, whatever its grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginDenial {
    /// The command is never granted to a plugin.
    Never {
        /// The command's name, or for a hotkey, `hotkey.set` binding it.
        command: String,
    },
    /// A batch with no edits.
    NoCommand,
}

/// The classes `call` needs: one per command it carries (each edit of a batch, or the
/// one action, query or history op). A `hotkey.set` also needs the class of the action
/// it binds, since that action runs when a person presses the chord: with `edit` alone a
/// plugin can't bind `output.start`. Fails for a call no plugin may make.
pub fn plugin_classes(call: &Call) -> Result<BTreeSet<PluginClass>, PluginDenial> {
    let mut names: Vec<(String, &'static str)> = Vec::new();
    match call {
        Call::Batch(batch) => {
            for edit in &batch.edits {
                names.push((edit.name().to_owned(), edit.name()));
                if let Edit::HotkeySet { action, .. } = edit {
                    let bound = format!("hotkey.set binding {}", action.name());
                    names.push((bound, action.name()));
                }
            }
        }
        Call::Action { action } => names.push((action.name().to_owned(), action.name())),
        Call::Query { query } => names.push((query.name().to_owned(), query.name())),
        Call::History(history) => names.push((history.op.name().to_owned(), history.op.name())),
    }
    if names.is_empty() {
        return Err(PluginDenial::NoCommand);
    }
    let mut classes = BTreeSet::new();
    for (command, name) in names {
        match plugin_scope(name) {
            PluginScope::Class(class) => {
                classes.insert(class);
            }
            PluginScope::Never => return Err(PluginDenial::Never { command }),
        }
    }
    Ok(classes)
}

/// What a person grants one plugin (ADR-0069): its capability classes, and the network
/// hosts and folders it may reach. A class never widens network or filesystem reach.
///
/// ```json
/// {"classes": ["edit", "live"], "hosts": ["https://api.example.com:443"], "folders": []}
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PluginGrant {
    /// The classes granted, besides `read`, which every plugin has. Omitted for none.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub classes: BTreeSet<PluginClass>,
    /// The hosts the plugin may reach, each as `scheme://host:port` (`http`, `https`,
    /// `ws` or `wss`). Omitted for none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    /// The folders the person picked for it, as absolute paths on the engine's machine,
    /// besides its own private data folder. Omitted for none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
}

impl PluginGrant {
    /// Whether this grant covers `class`. `read` is always covered.
    pub fn allows(&self, class: PluginClass) -> bool {
        class == PluginClass::Read || self.classes.contains(&class)
    }

    /// Checks a plugin's call against this grant: `Ok` when every class it needs is
    /// granted, otherwise the `forbidden` error the plugin gets. The call never reaches
    /// the engine either way it fails.
    pub fn check(&self, call: &Call) -> Result<(), ProtocolError> {
        let name = call_name(call);
        match plugin_classes(call) {
            Ok(classes) => match classes.into_iter().find(|class| !self.allows(*class)) {
                None => Ok(()),
                Some(class) => Err(ProtocolError::Forbidden {
                    call: name,
                    reason: format!("the plugin is not granted `{class}`"),
                }),
            },
            Err(PluginDenial::Never { command }) => Err(ProtocolError::Forbidden {
                call: name,
                reason: format!("`{command}` is never granted to a plugin"),
            }),
            Err(PluginDenial::NoCommand) => Err(ProtocolError::Forbidden {
                call: name,
                reason: "the call names no command a plugin may make".into(),
            }),
        }
    }

    /// Why an engine refuses this grant, as the field and the reason, or `None`: a host
    /// that isn't `scheme://host:port` with a known scheme and a port in 1..=65535, or a
    /// folder that isn't an absolute path.
    pub fn problem(&self) -> Option<(String, String)> {
        for (i, host) in self.hosts.iter().enumerate() {
            if let Some(reason) = host_problem(host) {
                return Some((format!("hosts.{i}"), reason.into()));
            }
        }
        for (i, folder) in self.folders.iter().enumerate() {
            if !is_absolute(folder) {
                return Some((format!("folders.{i}"), "must be an absolute path".into()));
            }
        }
        None
    }
}

/// The name a `forbidden` error names: the call's first command, or `batch` for an
/// empty one.
fn call_name(call: &Call) -> String {
    match call {
        Call::Batch(batch) => batch
            .edits
            .first()
            .map_or("batch", |edit| edit.name())
            .to_owned(),
        Call::Action { action } => action.name().to_owned(),
        Call::Query { query } => query.name().to_owned(),
        Call::History(history) => history.op.name().to_owned(),
    }
}

fn host_problem(host: &str) -> Option<&'static str> {
    let Some((scheme, rest)) = host.split_once("://") else {
        return Some("must be `scheme://host:port`");
    };
    if !["http", "https", "ws", "wss"].contains(&scheme) {
        return Some("must use `http`, `https`, `ws` or `wss`");
    }
    let Some((name, port)) = rest.rsplit_once(':') else {
        return Some("must name its port");
    };
    if name.is_empty() || name.contains(['/', '*', '@', '?', '#']) {
        return Some("must name one host, with no path, wildcard or credentials");
    }
    match port.parse::<u16>() {
        Ok(1..) => None,
        _ => Some("must have a port in 1..=65535"),
    }
}

/// An absolute path in either the Unix (`/data`) or Windows (`C:\data`, `\\server\share`)
/// form, since the grant names a path on the engine's machine, which the client may not
/// share.
fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'))
}

/// Whether a plugin runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PluginState {
    /// It is loaded and gets its events.
    Enabled,
    /// It doesn't run, until a person re-enables it (`plugin.enable`).
    Disabled,
}

/// Why the engine disabled a plugin. The engine never re-enables one by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PluginDisabledReason {
    /// It ran past its wall-time deadline.
    CpuLimit,
    /// It hit its memory or table cap.
    MemoryLimit,
    /// It trapped on its own fault.
    Trap,
    /// An update asks for more than its grant: a person must grant the difference.
    NeedsGrant,
    /// A reason this version of the protocol doesn't know. Never sent.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// Whether binding `action` to a hotkey is within `grant`: the check `plugin_classes`
/// makes for `hotkey.set`, for a host that binds on a plugin's behalf.
pub fn plugin_may_bind(action: &Action, grant: &PluginGrant) -> bool {
    matches!(plugin_scope(action.name()), PluginScope::Class(class) if grant.allows(class))
}

#[cfg(test)]
mod tests {
    use super::{
        PLUGIN_SCOPES, PluginClass, PluginDenial, PluginGrant, PluginScope, plugin_classes,
        plugin_may_bind, plugin_scope,
    };
    use crate::command::{Action, Batch, Edit};
    use crate::envelope::Call;
    use crate::error::ProtocolError;
    use crate::history::{HistoryCall, HistoryOp};
    use crate::hotkey::Chord;
    use crate::id::{OutputId, SceneId};
    use crate::query::Query;
    use std::collections::BTreeSet;

    fn bind(action: Action) -> Call {
        Call::Batch(Batch::single(Edit::HotkeySet {
            chord: Chord::new("Ctrl+1"),
            action,
        }))
    }

    fn grant(classes: &[PluginClass]) -> PluginGrant {
        PluginGrant {
            classes: classes.iter().copied().collect(),
            ..PluginGrant::default()
        }
    }

    #[test]
    fn the_table_names_each_command_once() {
        let mut seen = BTreeSet::new();
        for (name, _) in PLUGIN_SCOPES {
            assert!(seen.insert(*name), "{name} is placed twice");
        }
    }

    #[test]
    fn an_unplaced_command_is_never_granted() {
        assert_eq!(plugin_scope("future.command"), PluginScope::Never);
    }

    #[test]
    fn calls_need_the_classes_of_their_commands() {
        let start = Call::Action {
            action: Action::OutputStart {
                output_id: OutputId::new("main"),
            },
        };
        assert_eq!(
            plugin_classes(&start),
            Ok(BTreeSet::from([PluginClass::Outputs]))
        );
        let read = Call::Query {
            query: Query::StateGet,
        };
        assert_eq!(
            plugin_classes(&read),
            Ok(BTreeSet::from([PluginClass::Read]))
        );
        let undo = Call::History(HistoryCall::new(HistoryOp::Undo));
        assert_eq!(
            plugin_classes(&undo),
            Ok(BTreeSet::from([PluginClass::Edit]))
        );
        let key = Call::Action {
            action: Action::OutputClearKey {
                output_id: OutputId::new("main"),
            },
        };
        assert_eq!(
            plugin_classes(&key),
            Err(PluginDenial::Never {
                command: "output.clear_key".into()
            })
        );
        let empty = Call::Batch(Batch {
            edits: Vec::new(),
            ..Batch::single(Edit::CanvasSetSize {
                width: 1,
                height: 1,
            })
        });
        assert_eq!(plugin_classes(&empty), Err(PluginDenial::NoCommand));
    }

    #[test]
    fn a_hotkey_needs_the_class_of_the_action_it_binds() {
        let switch = bind(Action::SceneSwitch {
            scene_id: SceneId::new("a"),
        });
        assert_eq!(
            plugin_classes(&switch),
            Ok(BTreeSet::from([PluginClass::Edit, PluginClass::Live]))
        );
        let edit_only = grant(&[PluginClass::Edit]);
        let refused = edit_only.check(&switch).expect_err("needs live");
        assert!(matches!(&refused, ProtocolError::Forbidden { call, .. } if call == "hotkey.set"));
        assert!(
            grant(&[PluginClass::Edit, PluginClass::Live])
                .check(&switch)
                .is_ok()
        );

        let start = Action::OutputStart {
            output_id: OutputId::new("main"),
        };
        assert!(!plugin_may_bind(&start, &edit_only));
        assert!(plugin_may_bind(&start, &grant(&[PluginClass::Outputs])));
        assert!(edit_only.check(&bind(start)).is_err());

        // A never-granted action can't be bound with every class.
        let all = grant(&[PluginClass::Edit, PluginClass::Live, PluginClass::Outputs]);
        let clear = bind(Action::OutputClearKey {
            output_id: OutputId::new("main"),
        });
        assert_eq!(
            plugin_classes(&clear),
            Err(PluginDenial::Never {
                command: "hotkey.set binding output.clear_key".into()
            })
        );
        assert!(all.check(&clear).is_err());
    }

    #[test]
    fn read_is_always_granted() {
        let none = PluginGrant::default();
        assert!(none.allows(PluginClass::Read));
        assert!(!none.allows(PluginClass::Edit));
        let read = Call::Query {
            query: Query::SceneList,
        };
        assert!(none.check(&read).is_ok());
    }

    #[test]
    fn grants_check_hosts_and_folders() {
        let ok = PluginGrant {
            classes: BTreeSet::new(),
            hosts: vec![
                "https://api.example.com:443".into(),
                "ws://[::1]:8080".into(),
            ],
            folders: vec!["/home/me/clips".into(), r"C:\Clips".into()],
        };
        assert_eq!(ok.problem(), None);
        for (host, field) in [
            ("api.example.com:443", "hosts.0"),
            ("ftp://example.com:21", "hosts.0"),
            ("https://example.com", "hosts.0"),
            ("https://*.example.com:443", "hosts.0"),
            ("https://example.com:443/path", "hosts.0"),
            ("https://example.com:0", "hosts.0"),
        ] {
            let bad = PluginGrant {
                hosts: vec![host.into()],
                ..PluginGrant::default()
            };
            assert_eq!(bad.problem().map(|(f, _)| f), Some(field.into()), "{host}");
        }
        let relative = PluginGrant {
            folders: vec!["clips".into()],
            ..PluginGrant::default()
        };
        assert_eq!(relative.problem().map(|(f, _)| f), Some("folders.0".into()));
    }

    #[test]
    fn grants_parse_with_missing_fields() {
        let grant: PluginGrant = serde_json::from_str(r#"{"classes": ["edit"]}"#).expect("parses");
        assert_eq!(grant.classes, BTreeSet::from([PluginClass::Edit]));
        assert_eq!(
            serde_json::to_value(PluginGrant::default()).expect("serializes"),
            serde_json::json!({})
        );
    }
}
