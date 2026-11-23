//! "Control PTZ cameras" ([ADR-0123](../../../docs/adr/0123-lan-inputs-jtunnel-and-visca.md)
//! §4, [ADR-0146](../../../docs/adr/0146-ptz-cameras-shots-and-actions.md) §1,
//! [ADR-0209](../../../docs/adr/0209-value-bindings-aim-ptz-cameras.md) decision 1): the
//! desktop app's switch for driving PTZ heads, with the machine's camera list, as the
//! daemon's `--ptz-camera` gives it.
//!
//! - **The setting** is `ptz_cameras.json` in the data folder, user-only: `{"on": true,
//!   "cameras": [{"name": "Wide", "address": "192.168.1.40:52381", "transport":
//!   "visca_ip", "visca": 1}]}`. Only the person changes it, from the app's own window:
//!   the page calls `set_ptz_cameras` through `setPtzCameras` on `__JSTREAM_DESKTOP__`
//!   (`bootstrap.rs`), a Tauri command no engine client (an agent, a plugin or a paired
//!   device) can reach, so nothing but the person points JStream at an address. Per
//!   machine, never in the collection, which names a head only by its name. A missing or
//!   unreadable file means off.
//! - **It applies at once.** Saving it gives the engine the new list
//!   (`Engine::start_ptz`), which stops the heads it no longer lists, each sending its
//!   stop first. Off, or on with no camera, no PTZ socket is open. A list the engine
//!   refuses (an address off the LAN, a VISCA address outside 1 to 7, two cameras with
//!   one name or on one head) is never saved.
//!
//! Docs: apps/jstream-desktop/README.md#design

use jstream_core::{Engine, PtzCamera, PtzSetup};
use jstream_protocol::PtzTransport;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Weak;

/// The setting's file in the data folder.
pub const PTZ_CAMERAS_FILE: &str = "ptz_cameras.json";

/// The setting's file in `data`.
pub fn setting_file(data: &Path) -> PathBuf {
    data.join(PTZ_CAMERAS_FILE)
}

/// One camera as the file and the page hold it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Camera {
    /// Its name, which shots, `ptz.*` actions and value bindings take.
    pub name: String,
    /// Its address and port, as `192.168.1.40:52381`: a LAN address.
    pub address: String,
    /// `visca_ip`, `udp` or `tcp`.
    pub transport: String,
    /// Its VISCA address on that link, 1 to 7.
    pub visca: u8,
}

impl Camera {
    /// The engine's camera, or why it refuses it.
    ///
    /// # Errors
    /// An address or transport it can't read, or a camera the engine refuses
    /// ([`PtzCamera::new`]).
    pub fn camera(&self) -> Result<PtzCamera, String> {
        let addr = self
            .address
            .trim()
            .parse()
            .map_err(|_| format!("`{}` is not an address and port", self.address.trim()))?;
        let transport = match self.transport.as_str() {
            "visca_ip" => PtzTransport::ViscaIp,
            "udp" => PtzTransport::Udp,
            "tcp" => PtzTransport::Tcp,
            other => {
                return Err(format!(
                    "`{other}` is not a transport: visca_ip, udp or tcp"
                ));
            }
        };
        PtzCamera::new(&self.name, addr, transport, self.visca)
            .map_err(|error| format!("camera `{}`: {error}", self.name))
    }
}

/// The setting: whether JStream drives PTZ cameras on this machine, and which.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Setting {
    /// "Control PTZ cameras".
    pub on: bool,
    /// The cameras, in the order the person added them.
    pub cameras: Vec<Camera>,
}

impl Setting {
    /// The engine's setup: empty while off.
    ///
    /// # Errors
    /// A camera or a list the engine refuses ([`Camera::camera`], [`PtzSetup::new`]),
    /// named in the message. Checked while off too, so what is saved always starts.
    pub fn setup(&self) -> Result<PtzSetup, String> {
        let cameras = self
            .cameras
            .iter()
            .map(Camera::camera)
            .collect::<Result<Vec<_>, String>>()?;
        let setup = PtzSetup::new(cameras)?;
        Ok(if self.on { setup } else { PtzSetup::default() })
    }
}

/// The setting in `file`. A missing file is off; one that can't be read or parsed is
/// off too, and logged.
pub fn read(file: &Path) -> Setting {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Setting::default(),
        Err(error) => {
            tracing::warn!(file = %file.display(), %error, "could not read the PTZ camera setting; it stays off");
            return Setting::default();
        }
    };
    // An object only: serde would also read a struct from an array.
    let parsed = serde_json::from_str::<serde_json::Value>(&text).and_then(|value| {
        if value.is_object() {
            serde_json::from_value::<Setting>(value)
        } else {
            Err(serde::de::Error::custom("not an object"))
        }
    });
    match parsed {
        Ok(setting) => setting,
        Err(error) => {
            tracing::warn!(file = %file.display(), %error, "the PTZ camera setting is not valid; it stays off");
            Setting::default()
        }
    }
}

/// Saves `setting`, user-only, replacing the file whole, once the engine would take it.
///
/// # Errors
/// A camera the engine refuses, or a file that can't be written.
pub fn write(file: &Path, setting: &Setting) -> Result<(), String> {
    setting.setup()?;
    let text = serde_json::to_string(setting).map_err(|err| err.to_string())?;
    let temporary = file.with_extension("json.tmp");
    // A leftover keeps its own mode when opened again, so it goes first (#1446 N1).
    match std::fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.to_string()),
    }
    crate::network::write_private(&temporary, text.as_bytes()).map_err(|err| err.to_string())?;
    std::fs::rename(&temporary, file).map_err(|err| err.to_string())
}

/// Gives `engine` the cameras `setting` lists, or none while off.
///
/// # Errors
/// A setting the engine refuses, or a camera's socket that can't be opened; the
/// cameras running before are kept then.
pub fn apply(engine: &Engine, setting: &Setting) -> Result<(), String> {
    let setup = setting.setup()?;
    engine
        .start_ptz(&setup)
        .map_err(|error| format!("could not open a PTZ camera's connection: {error}"))?;
    if !setup.cameras().is_empty() {
        let cameras: Vec<String> = setup.cameras().iter().map(ToString::to_string).collect();
        tracing::info!(cameras = ?cameras, "controlling PTZ cameras");
    }
    Ok(())
}

/// Starts PTZ control as `file` says, at startup. A setting the engine refuses, or a
/// socket that can't be opened, is logged, and the app runs without it.
pub fn start(engine: &Engine, file: &Path) {
    let setting = read(file);
    if !setting.on || setting.cameras.is_empty() {
        return;
    }
    if let Err(error) = apply(engine, &setting) {
        tracing::error!(%error, "PTZ camera control stays off");
    }
}

/// `set_ptz_cameras`: saves the setting from the app's own window and [`apply`]s it at
/// once. Answers `null` when it was saved and applied, or why not, for the page to
/// show.
#[cfg(webview)]
#[tauri::command]
pub(crate) fn set_ptz_cameras(file: tauri::State<'_, File>, setting: Setting) -> Option<String> {
    if let Err(error) = write(&file.path, &setting) {
        tracing::warn!(%error, "could not save the PTZ camera setting");
        return Some(error);
    }
    tracing::info!(
        on = setting.on,
        cameras = setting.cameras.len(),
        "PTZ camera setting saved"
    );
    let engine = file.engine.upgrade()?;
    match apply(&engine, &setting) {
        Ok(()) => None,
        Err(error) => {
            tracing::warn!(%error, "the PTZ camera setting was saved but didn't apply");
            Some(format!("Saved, but it didn't apply: {error}"))
        }
    }
}

/// The setting's file, the state `set_ptz_cameras` writes, and the engine it applies
/// to. A weak handle, so the host still lets the engine go last when the app exits.
pub struct File {
    /// The setting's file.
    pub path: PathBuf,
    /// The engine, while it runs.
    pub engine: Weak<Engine>,
}

#[cfg(test)]
mod tests {
    use super::{Camera, Setting, apply, read, setting_file, start, write};

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jstream-desktop-ptz-cameras-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp folder");
        dir
    }

    fn camera(name: &str, address: &str, transport: &str) -> Camera {
        Camera {
            name: name.into(),
            address: address.into(),
            transport: transport.into(),
            visca: 1,
        }
    }

    fn listed(engine: &jstream_core::Engine) -> Vec<String> {
        use jstream_control::Control as _;
        use jstream_protocol::{Call, CallResult, Query, QueryResult};
        let control = engine.connect();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        let reply = runtime.block_on(async {
            control
                .hello(jstream_protocol::Hello {
                    protocol: jstream_protocol::PROTOCOL_VERSION,
                    client_name: "test".into(),
                })
                .await
                .expect("hello");
            control
                .call(Call::Query {
                    query: Query::PtzList,
                })
                .await
        });
        match reply.map(|r| r.result) {
            Ok(CallResult::Query {
                result: QueryResult::PtzList { cameras },
            }) => cameras.into_iter().map(|c| c.camera).collect(),
            other => panic!("ptz.list answered {other:?}"),
        }
    }

    /// ADR-0123 §4, ADR-0209 decision 1: off until the person turns it on; a saved list
    /// applies at once, and off stops every head.
    #[test]
    fn the_camera_list_is_off_until_turned_on_and_applies_at_once() {
        let dir = folder("round-trip");
        let file = setting_file(&dir);
        assert_eq!(read(&file), Setting::default(), "a missing file is off");
        let engine = jstream_core::Engine::new();
        start(&engine, &file);
        assert!(listed(&engine).is_empty(), "off drives nothing");
        let on = Setting {
            on: true,
            cameras: vec![
                camera("Wide", "127.0.0.1:52381", "visca_ip"),
                camera("Close", "127.0.0.1:5678", "udp"),
            ],
        };
        write(&file, &on).expect("saved");
        assert_eq!(read(&file), on);
        apply(&engine, &on).expect("applied");
        assert_eq!(listed(&engine), ["Wide", "Close"]);
        let off = Setting {
            on: false,
            ..on.clone()
        };
        write(&file, &off).expect("saved");
        apply(&engine, &off).expect("applied");
        assert!(listed(&engine).is_empty(), "off stops them");
        assert_eq!(read(&file).cameras.len(), 2, "off keeps the list");
        // A fresh start reads the file.
        write(&file, &on).expect("saved");
        let again = jstream_core::Engine::new();
        start(&again, &file);
        assert_eq!(listed(&again), ["Wide", "Close"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&file)
                .expect("there")
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "user-only");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the engine refuses is never saved, even while off, and a malformed file is
    /// off.
    #[test]
    fn a_refused_camera_isnt_saved_and_a_malformed_file_is_off() {
        let dir = folder("refused");
        let file = setting_file(&dir);
        let mut far_visca = camera("Wide", "192.168.1.40:52381", "visca_ip");
        far_visca.visca = 8;
        for cameras in [
            vec![camera("Wide", "8.8.8.8:52381", "visca_ip")],
            vec![camera("Wide", "not an address", "visca_ip")],
            vec![camera("Wide", "192.168.1.40:52381", "serial")],
            vec![camera("", "192.168.1.40:52381", "visca_ip")],
            vec![camera("Wide", "192.168.1.40:0", "udp")],
            vec![far_visca],
            vec![
                camera("Wide", "192.168.1.40:52381", "visca_ip"),
                camera("Wide", "192.168.1.41:52381", "visca_ip"),
            ],
            vec![
                camera("Wide", "192.168.1.40:52381", "visca_ip"),
                camera("Close", "192.168.1.40:52381", "udp"),
            ],
        ] {
            let mut setting = Setting { on: true, cameras };
            assert!(write(&file, &setting).is_err(), "{setting:?}");
            setting.on = false;
            assert!(write(&file, &setting).is_err(), "off too: {setting:?}");
        }
        assert!(!file.exists(), "nothing was saved");
        for text in ["not json", "[true]", r#"{"on":"yes"}"#] {
            std::fs::write(&file, text).expect("written");
            assert_eq!(read(&file), Setting::default(), "{text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
