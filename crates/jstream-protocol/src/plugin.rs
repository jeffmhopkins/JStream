//! WASM automation plugins on the wire (ADR-0069): the capability classes a person
//! grants a plugin, the grant itself, a plugin's state, and the table that places every
//! protocol command in one class or under "never".
//!
//! A plugin calls the API through a scoped `Control` that checks each call with
//! [`PluginGrant::check`] (its classes, from [`plugin_classes`], and the hosts and files
//! its settings name, from [`settings_reach`]) before it reaches the engine, and a call
//! outside the plugin's grant fails with `forbidden`. The table lives here, beside the commands, so a new command
//! can't ship unplaced: `every_command_has_a_plugin_scope` fails until a PR places it, and
//! until then it is "never".

use crate::command::{Action, Edit};
use crate::envelope::Call;
use crate::error::ProtocolError;
use crate::settings::{AssetRoot, Reference, Settings};
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
    /// The scene, item, source, filter, transition, bus, send, hotkey, canvas and studio
    /// edits, live parameter edits (`live.*`), and undo and redo on the plugin's own
    /// session.
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
    ("source.sizes", Class(R)),
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
    ("privacy.get", Class(R)),
    // The pixels of a user's capture could leak through any granted host; with no
    // filesystem grant there is nowhere safe for them.
    ("video.snapshot", N),
    ("youtube.status", N),
    ("youtube.list_broadcasts", N),
    ("youtube.list_streams", N),
    ("youtube.stats", N),
    ("device.list", N),
    // What other clients did, by name, is the user's to read, not a plugin's (ADR-0083).
    ("activity.list", N),
    // The server answers it, and a UI's settings are the person's.
    ("ui_settings.get", N),
    // Edits.
    ("scene.create", Class(E)),
    ("scene.rename", Class(E)),
    ("scene.set_role", Class(E)),
    ("scene.delete", Class(E)),
    ("scene.reorder", Class(E)),
    ("scene.set_folder", Class(E)),
    ("scene.set_meta", Class(E)),
    ("scene.set_transitions", Class(E)),
    ("scene.create_variant", Class(E)),
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
    ("item.set_transition", Class(E)),
    ("item.clear_override", Class(E)),
    ("source.create", Class(E)),
    ("source.rename", Class(E)),
    ("source.set_settings", Class(E)),
    ("source.duplicate", Class(E)),
    ("source.delete", Class(E)),
    ("source.set_audio", Class(E)),
    ("source.set_meta", Class(E)),
    ("filter.add", Class(E)),
    ("filter.remove", Class(E)),
    ("filter.set_enabled", Class(E)),
    ("filter.set_settings", Class(E)),
    ("filter.reorder", Class(E)),
    ("bus.create", Class(E)),
    ("bus.rename", Class(E)),
    ("bus.set_fader", Class(E)),
    ("bus.set_monitor", Class(E)),
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
    // Transition edits are in `edit` (ADR-0069 S2 names them); their settings are
    // checked for reach like a source's.
    ("transition.create", Class(E)),
    ("transition.update", Class(E)),
    ("transition.delete", Class(E)),
    ("transition.set_default", Class(E)),
    // Not named in any class by ADR-0069 either, so "never" until a PR places them.
    ("folder.create", N),
    ("folder.rename", N),
    ("folder.set_parent", N),
    ("folder.reorder", N),
    ("folder.delete", N),
    ("label.create", N),
    ("label.update", N),
    ("label.reorder", N),
    ("label.delete", N),
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
    ("ui_settings.set", N),
    // Needs a filesystem grant, like `video.snapshot`.
    ("preview.subscribe", N),
    // Not named in any class, so "never" until a PR places them.
    ("preview.unsubscribe", N),
    ("audio.reset_loudness", N),
    // A studio edit (ADR-0069 S2): it puts a scene on preview, not on air.
    ("studio.set_preview", Class(E)),
    // Deprecated (ADR-0080): the engine refuses it anyway.
    ("privacy.set", N),
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
        Call::Action { action } => {
            names.push((action.name().to_owned(), action.name()));
            // A live value is a setter edit, so it needs that edit's class too.
            if let Action::LiveSet { values, .. } = action {
                for edit in values {
                    let inner = format!("live.set of {}", edit.name());
                    names.push((inner, edit.name()));
                }
            }
        }
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
/// hosts and folders it may reach. A class never widens network or filesystem reach:
/// a call's URLs and files must be within `hosts` and `folders`.
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
    /// The folders the person picked for it, as absolute paths on the engine's machine.
    /// Omitted for none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
}

impl PluginGrant {
    /// Whether this grant covers `class`. `read` is always covered.
    pub fn allows(&self, class: PluginClass) -> bool {
        class == PluginClass::Read || self.classes.contains(&class)
    }

    /// Checks a plugin's call against this grant: `Ok` when every class it needs is
    /// granted and every host and file its settings name is within [`hosts`] and
    /// [`folders`] (see [`settings_reach`]), otherwise the `forbidden` error the plugin
    /// gets. The call never reaches the engine either way it fails.
    ///
    /// [`hosts`]: PluginGrant::hosts
    /// [`folders`]: PluginGrant::folders
    pub fn check(&self, call: &Call) -> Result<(), ProtocolError> {
        let name = call_name(call);
        match plugin_classes(call) {
            Ok(classes) => match classes.into_iter().find(|class| !self.allows(*class)) {
                None => match self.reach_problem(call) {
                    None => Ok(()),
                    Some(reason) => Err(ProtocolError::Forbidden { call: name, reason }),
                },
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

    /// Why the hosts and files `call`'s settings name aren't all within this grant, or
    /// `None` when they are.
    fn reach_problem(&self, call: &Call) -> Option<String> {
        let mut edits: Vec<&Edit> = Vec::new();
        match call {
            Call::Batch(batch) => edits.extend(&batch.edits),
            Call::Action {
                action: Action::LiveSet { values, .. },
            } => edits.extend(values),
            _ => {}
        }
        for edit in edits {
            let settings = match edit {
                Edit::SourceCreate { settings, .. }
                | Edit::FilterAdd { settings, .. }
                | Edit::TransitionCreate { settings, .. } => settings,
                Edit::SourceSetSettings { patch, .. }
                | Edit::FilterSetSettings { patch, .. }
                | Edit::TransitionUpdate { patch, .. } => patch,
                _ => continue,
            };
            for reach in settings_reach(settings) {
                let allowed = match &reach {
                    Reach::Host(host) => self
                        .hosts
                        .iter()
                        .any(|granted| granted.eq_ignore_ascii_case(host)),
                    Reach::File(path) => self.folders.iter().any(|folder| is_under(path, folder)),
                    Reach::Unchecked(_) => false,
                };
                if !allowed {
                    return Some(match reach {
                        Reach::Host(host) => format!("the plugin is not granted the host `{host}`"),
                        Reach::File(path) => {
                            format!("the plugin is not granted a folder holding `{path}`")
                        }
                        Reach::Unchecked(what) => format!("a plugin can't name {what}"),
                    });
                }
            }
        }
        None
    }

    /// Why an engine refuses this grant, as the field and the reason, or `None`: a host
    /// that isn't `scheme://host:port` with a known scheme and a port in 1..=65535, or a
    /// folder that isn't an absolute path or has a `.` or `..` segment. Both path forms
    /// pass, since the client may not share the engine's OS: an engine also refuses a
    /// folder its own OS doesn't read as absolute (`/data` on Windows, where it is
    /// relative to the current drive).
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
            if has_dot_segment(folder) {
                return Some((
                    format!("folders.{i}"),
                    "must have no `.` or `..` segment".into(),
                ));
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

/// A host or file a source's or filter's settings would make the engine reach.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reach {
    /// A network host, as `scheme://host:port` with the host in lower case and the
    /// scheme's default port filled in: the form [`PluginGrant::hosts`] holds.
    Host(String),
    /// An absolute file path.
    File(String),
    /// Something a grant can't name, so no plugin may reach it: a file under the
    /// collection's or the app's folder, or a URL that isn't plainly `scheme://host:port`
    /// with a known scheme. Says what it is.
    Unchecked(String),
}

/// What a settings map (a create's settings or a patch) makes the engine reach, read
/// without its kind's descriptor (ADR-0069 S2: a `browser` source's URL, and the file
/// settings of media, image and other kinds, need the matching grant beside `edit`):
/// - every asset reference (`{"type":"asset",…}`) is a [`Reach::File`] under the
///   `absolute` root, and [`Reach::Unchecked`] under `collection` or `app`;
/// - every string value that starts as an absolute URL does, `scheme://` after
///   the clean-up a URL parser or the engine applies (outer spaces, control characters
///   and white space trimmed, tabs and newlines removed), is a [`Reach::Host`] when it parses as an `http`, `https`,
///   `ws` or `wss` URL with a host and no credentials, and [`Reach::Unchecked`] otherwise.
///   Text with a `://` later in it ("Follow me at https://…") is no URL a parser or
///   the engine would load, so it reaches nothing.
///
/// Values nested in arrays and objects count too. A value without `://` that the engine
/// would still resolve to a host (a `youtube_chat` video ID) is not found here.
pub fn settings_reach(settings: &Settings) -> Vec<Reach> {
    let mut found = Vec::new();
    for value in settings.0.values() {
        value_reach(value, &mut found);
    }
    found
}

fn value_reach(value: &serde_json::Value, found: &mut Vec<Reach>) {
    use serde_json::Value;
    if let Some(Reference::Asset { root, path }) = Reference::from_value(value) {
        found.push(match root {
            AssetRoot::Absolute => Reach::File(path),
            AssetRoot::Collection => {
                Reach::Unchecked("a file under the collection's folder".into())
            }
            _ => Reach::Unchecked("a file under the app's folder".into()),
        });
        return;
    }
    match value {
        Value::String(text) => {
            if let Some(url) = url_text(text) {
                found.push(url_reach(&url));
            }
        }
        Value::Array(items) => items.iter().for_each(|item| value_reach(item, found)),
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("asset") {
                // Tagged as a file but not one: it can't be checked, so it isn't let through.
                found.push(Reach::Unchecked("a malformed file reference".into()));
            } else {
                map.values().for_each(|item| value_reach(item, found));
            }
        }
        _ => {}
    }
}

/// `text` as a URL parser or the engine reads it, when it starts with `scheme://`:
/// leading and trailing ASCII spaces, control characters and Unicode white space dropped
/// (the WHATWG URL parser trims the first two, and the engine's `str::trim`, as in the
/// browser source's page location, the last), and tabs and newlines removed anywhere,
/// with a scheme of an ASCII letter then letters, digits, `+`, `-` or `.`. Trimming the
/// union of what every consumer trims keeps a string the engine would load from slipping
/// past the check (#642 review B2). `None` for text that doesn't start so, which no
/// consumer takes for an absolute URL.
fn url_text(text: &str) -> Option<String> {
    let cleaned: String = text
        .trim_matches(|c: char| c <= ' ' || c.is_whitespace())
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let (scheme, _) = cleaned.split_once("://")?;
    let mut chars = scheme.chars();
    let starts = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    let rest = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    (starts && rest).then_some(cleaned)
}

/// The `scheme://host:port` a URL reaches, filling in the scheme's default port.
fn url_reach(url: &str) -> Reach {
    let unchecked = || Reach::Unchecked(format!("the address `{url}`"));
    let Some((scheme, rest)) = url.trim().split_once("://") else {
        return unchecked();
    };
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" | "ws" => 80,
        "https" | "wss" => 443,
        _ => return unchecked(),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains(['@', '\\', '%']) {
        return unchecked();
    }
    let (host, port) = match authority.rsplit_once(':') {
        // An IPv6 literal's colons are inside its brackets.
        Some((host, port)) if !port.contains(']') => match port.parse::<u16>() {
            Ok(port @ 1..) => (host, port),
            _ => return unchecked(),
        },
        _ => (authority, default_port),
    };
    if host.is_empty() {
        return unchecked();
    }
    Reach::Host(format!("{scheme}://{}:{port}", host.to_ascii_lowercase()))
}

/// Whether the absolute `path` is `folder` or inside it, comparing `/` and `\\` alike, and
/// letters without case when `folder` is a Windows path (a drive letter or UNC). A path
/// with a `.` or `..` segment is never inside.
fn is_under(path: &str, folder: &str) -> bool {
    if has_dot_segment(path) {
        return false;
    }
    let windows = !folder.starts_with('/');
    let norm = |s: &str| {
        let s = s.replace('\\', "/");
        let s = s.trim_end_matches('/').to_owned();
        if windows { s.to_ascii_lowercase() } else { s }
    };
    let (path, folder) = (norm(path), norm(folder));
    path == folder
        || path
            .strip_prefix(&folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn has_dot_segment(path: &str) -> bool {
    path.split(['/', '\\'])
        .any(|segment| segment == "." || segment == "..")
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
        PLUGIN_SCOPES, PluginClass, PluginDenial, PluginGrant, PluginScope, Reach, plugin_classes,
        plugin_may_bind, plugin_scope, settings_reach,
    };
    use crate::command::{Action, Batch, Edit, SourceKind};
    use crate::envelope::Call;
    use crate::error::ProtocolError;
    use crate::history::{HistoryCall, HistoryOp};
    use crate::hotkey::Chord;
    use crate::id::{GestureId, OutputId, SceneId, SourceId, TransitionPresetId};
    use crate::query::Query;
    use crate::settings::Settings;
    use crate::transition::Easing;
    use serde_json::json;
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
            preset_id: None,
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

    fn create(kind: &str, settings: serde_json::Value) -> Call {
        let settings: Settings = serde_json::from_value(settings).expect("a settings map");
        Call::Batch(Batch::single(Edit::SourceCreate {
            source_id: SourceId::new("s"),
            name: "S".into(),
            kind: SourceKind::Builtin { kind: kind.into() },
            settings,
            duplicated_from: None,
        }))
    }

    fn set(patch: serde_json::Value) -> Call {
        let patch: Settings = serde_json::from_value(patch).expect("a settings map");
        Call::Batch(Batch::single(Edit::SourceSetSettings {
            source_id: SourceId::new("s"),
            patch,
        }))
    }

    fn file(path: &str) -> serde_json::Value {
        json!({"type": "asset", "root": "absolute", "path": path})
    }

    fn forbidden(grant: &PluginGrant, call: &Call) -> bool {
        matches!(grant.check(call), Err(ProtocolError::Forbidden { .. }))
    }

    /// ADR-0069 S2: `edit` never widens network or filesystem reach.
    #[test]
    fn edit_alone_cant_reach_a_host_or_a_file() {
        let edit = grant(&[PluginClass::Edit]);
        let page = create("browser", json!({"url": "https://attacker.example/x"}));
        assert!(forbidden(&edit, &page));
        assert!(forbidden(
            &edit,
            &set(json!({"url": "https://attacker.example/"}))
        ));
        assert!(forbidden(
            &edit,
            &create("browser", json!({"file": file("/home/me/page.html")}))
        ));
        assert!(forbidden(
            &edit,
            &create("media", json!({"file": file("C:\\Users\\me\\a.mp4")}))
        ));
        assert!(forbidden(
            &edit,
            &set(json!({"file": file("/etc/passwd.png")}))
        ));
        // Through a live value too.
        let live = Call::Action {
            action: Action::LiveSet {
                gesture_id: GestureId::new("g"),
                values: vec![Edit::SourceSetSettings {
                    source_id: SourceId::new("s"),
                    patch: serde_json::from_value(json!({"file": file("/x.png")})).expect("map"),
                }],
            },
        };
        assert!(forbidden(&edit, &live));
        // A transition preset's settings are checked the same way.
        let preset = Call::Batch(Batch::single(Edit::TransitionCreate {
            preset_id: TransitionPresetId::new("t"),
            name: "T".into(),
            kind: "fade".into(),
            duration_ms: 300,
            easing: Easing::default(),
            settings: serde_json::from_value(json!({"file": file("/x.webm")})).expect("map"),
        }));
        assert!(forbidden(&edit, &preset));
        let patch = Call::Batch(Batch::single(Edit::TransitionUpdate {
            preset_id: TransitionPresetId::new("t"),
            name: None,
            kind: None,
            duration_ms: None,
            easing: None,
            patch: serde_json::from_value(json!({"url": "https://attacker.example/"}))
                .expect("map"),
        }));
        assert!(forbidden(&edit, &patch));
        // What reaches nothing is still an ordinary edit.
        assert!(
            edit.check(&create("color", json!({"color": "#112233"})))
                .is_ok()
        );
        assert!(edit.check(&create("browser", json!({}))).is_ok());
    }

    #[test]
    fn a_granted_host_or_folder_lets_the_edit_through() {
        let reach = PluginGrant {
            classes: BTreeSet::from([PluginClass::Edit]),
            hosts: vec!["https://overlay.example:443".into()],
            folders: vec!["/home/me/overlays".into(), "D:\\Media".into()],
        };
        let ok = |call: &Call| reach.check(call).is_ok();
        assert!(ok(&create(
            "browser",
            json!({"url": "https://Overlay.example/page?x=1"})
        )));
        assert!(ok(&set(json!({"url": "https://overlay.example:443"}))));
        assert!(
            !ok(&set(json!({"url": "http://overlay.example/"}))),
            "another port"
        );
        assert!(!ok(&set(json!({"url": "https://overlay.example.evil/"}))));
        assert!(
            !ok(&set(json!({"url": "https://me@overlay.example/"}))),
            "credentials"
        );
        assert!(
            !ok(&set(json!({"url": "file:///etc/passwd"}))),
            "another scheme"
        );
        assert!(ok(&set(json!({"file": file("/home/me/overlays/a.png")}))));
        assert!(ok(&set(json!({"file": file("d:/media/clips/a.mp4")}))));
        assert!(!ok(&set(
            json!({"file": file("/home/me/overlays-old/a.png")})
        )));
        assert!(!ok(&set(
            json!({"file": file("/home/me/overlays/../.ssh/id.png")})
        )));
        // Files under the collection's or the app's folder can't be granted.
        let relative = json!({"type": "asset", "root": "collection", "path": "a.png"});
        assert!(!ok(&set(json!({ "file": relative }))));
    }

    #[test]
    fn settings_reach_reads_urls_and_files_without_a_descriptor() {
        let settings: Settings = serde_json::from_value(json!({
            "url": "wss://[::1]:9000/socket",
            "list": ["http://a.example", {"nested": file("/f")}],
            "text": "no address here",
        }))
        .expect("a settings map");
        assert_eq!(
            settings_reach(&settings),
            vec![
                Reach::Host("http://a.example:80".into()),
                Reach::File("/f".into()),
                Reach::Host("wss://[::1]:9000".into()),
            ]
        );
    }

    /// Text with an address in it is no URL; what a parser would still take for one,
    /// after trimming and dropping tabs and newlines, is checked (#642 review N4).
    #[test]
    fn only_text_that_starts_as_a_url_reaches_a_host() {
        let reach = |text: &str| {
            let settings: Settings =
                serde_json::from_value(json!({ "v": text })).expect("a settings map");
            settings_reach(&settings)
        };
        assert_eq!(reach("Follow me at https://example.tv/me"), vec![]);
        assert_eq!(reach("now playing: song (http://x.example)"), vec![]);
        assert_eq!(
            reach("  HTTPS://Example.tv/me  "),
            vec![Reach::Host("https://example.tv:443".into())]
        );
        assert_eq!(
            reach("ht\ttps://evil.example/"),
            vec![Reach::Host("https://evil.example:443".into())],
            "a tab inside the scheme is dropped, as the parser drops it"
        );
        for space in ["\u{a0}", "\u{3000}", "\u{2003}", "\u{85}"] {
            assert_eq!(
                reach(&format!("{space}https://evil.example/{space}")),
                vec![Reach::Host("https://evil.example:443".into())],
                "Unicode white space is trimmed, as the engine's str::trim trims it (B2)"
            );
        }
        assert_eq!(
            reach("https://evil.example/a page with spaces"),
            vec![Reach::Host("https://evil.example:443".into())]
        );
        assert!(matches!(
            reach("file:///etc/passwd").as_slice(),
            [Reach::Unchecked(_)]
        ));
        assert!(matches!(
            reach("git+ssh://host/repo").as_slice(),
            [Reach::Unchecked(_)]
        ));
    }

    #[test]
    fn a_live_value_needs_its_edits_class() {
        let live = Call::Action {
            action: Action::LiveSet {
                gesture_id: GestureId::new("g"),
                values: vec![Edit::OutputSetVideoMix {
                    output_id: OutputId::new("main"),
                    mix_id: None,
                }],
            },
        };
        assert_eq!(
            plugin_classes(&live),
            Err(PluginDenial::Never {
                command: "live.set of output.set_video_mix".into()
            })
        );
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
        let dotted = PluginGrant {
            folders: vec!["/home/me/clips".into(), r"C:\Clips\..\Windows".into()],
            ..PluginGrant::default()
        };
        assert_eq!(dotted.problem().map(|(f, _)| f), Some("folders.1".into()));
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
