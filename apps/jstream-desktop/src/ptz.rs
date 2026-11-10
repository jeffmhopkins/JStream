//! "Accept a PTZ controller" ([ADR-0123](../../../docs/adr/0123-lan-inputs-jtunnel-and-visca.md)
//! §5, [ADR-0152](../../../docs/adr/0152-a-ptz-controller-drives-scene-cameras.md),
//! [ADR-0153](../../../docs/adr/0153-ptz-controller-idle-stop-counters-and-setting.md) §3):
//! the desktop app's switch for the VISCA controller endpoints, with the endpoints and
//! the controller addresses this machine allows, as the daemon's `--ptz-controller` and
//! `--ptz-controller-allow` give them.
//!
//! - **The setting** is `ptz_controllers.json` in the data folder, user-only:
//!   `{"on": true, "endpoints": [{"address": "192.168.1.20:52381", "transport":
//!   "visca_ip", "visca": 1, "target": "program"}], "allow": ["192.168.1.50"]}`. Only the
//!   person changes it, from the app's own window: the page calls `set_ptz_controllers`
//!   through `setPtz` on `__JSTREAM_DESKTOP__` (`bootstrap.rs`), a Tauri
//!   command no engine client (an agent, a plugin or a paired device) can reach. Per
//!   machine, never in the collection. A missing or unreadable file means off. It takes
//!   effect when the app next starts.
//! - **On**, the app opens the endpoints (`Engine::start_ptz_controllers`) and keeps them
//!   until it exits. On with no endpoint opens nothing. Off, no socket is open. A setting
//!   the engine refuses (an endpoint off the LAN or on every interface, no allowed
//!   address) is never saved.
//!
//! Docs: apps/jstream-desktop/README.md#design

use jstream_core::{
    Engine, PtzController, PtzControllerSetup, PtzControllerTarget, PtzControllers,
};
use jstream_protocol::{PtzTransport, SceneId};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

/// The setting's file in the data folder.
pub const PTZ_FILE: &str = "ptz_controllers.json";

/// The setting's file in `data`.
pub fn setting_file(data: &Path) -> PathBuf {
    data.join(PTZ_FILE)
}

/// One endpoint as the file and the page hold it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    /// The address and port it listens on, as `192.168.1.20:52381`: one of this
    /// machine's LAN addresses.
    pub address: String,
    /// `visca_ip`, `udp` or `tcp`.
    pub transport: String,
    /// The VISCA address it answers as, 1 to 7.
    pub visca: u8,
    /// `program`, `preview` or `scene:<scene id>`.
    pub target: String,
}

impl Endpoint {
    /// The engine's endpoint, or why it refuses it.
    ///
    /// # Errors
    /// An address, transport or target it can't read, or one the engine refuses
    /// ([`PtzController::new`]).
    pub fn controller(&self) -> Result<PtzController, String> {
        let bind = self
            .address
            .trim()
            .parse()
            .map_err(|err| format!("`{}` is not an address and port: {err}", self.address))?;
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
        let target = match self.target.as_str() {
            "program" => PtzControllerTarget::Program,
            "preview" => PtzControllerTarget::Preview,
            other => match other.strip_prefix("scene:") {
                Some(id) if !id.trim().is_empty() => {
                    PtzControllerTarget::Scene(SceneId::new(id.trim()))
                }
                _ => {
                    return Err(format!(
                        "`{other}` is not a target: program, preview or scene:<id>"
                    ));
                }
            },
        };
        PtzController::new(bind, transport, self.visca, target)
    }
}

/// The setting: whether the endpoints open, the endpoints, and the controller addresses
/// they answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Setting {
    /// "Accept a PTZ controller".
    pub on: bool,
    /// The endpoints, in the order the person added them.
    pub endpoints: Vec<Endpoint>,
    /// The controller addresses allowed, as `192.168.1.50`.
    pub allow: Vec<String>,
}

impl Setting {
    /// The endpoints' setup: `None` while off.
    ///
    /// # Errors
    /// An endpoint or address the engine refuses ([`Endpoint::controller`],
    /// [`PtzControllerSetup::new`]), named in the message.
    pub fn setup(&self) -> Result<Option<PtzControllerSetup>, String> {
        let controllers = self
            .endpoints
            .iter()
            .map(Endpoint::controller)
            .collect::<Result<Vec<_>, String>>()?;
        let allow = self
            .allow
            .iter()
            .map(|ip| {
                ip.trim()
                    .parse::<IpAddr>()
                    .map_err(|_| format!("`{ip}` is not an IP address"))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let setup = PtzControllerSetup::new(controllers, allow)?;
        Ok(self.on.then_some(setup))
    }
}

/// The setting in `file`. A missing file is off; one that can't be read or parsed is
/// off too, and logged.
pub fn read(file: &Path) -> Setting {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Setting::default(),
        Err(error) => {
            tracing::warn!(file = %file.display(), %error, "could not read the PTZ controller setting; it stays off");
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
            tracing::warn!(file = %file.display(), %error, "the PTZ controller setting is not valid; it stays off");
            Setting::default()
        }
    }
}

/// Saves `setting`, user-only, replacing the file whole, once the engine would take it.
///
/// # Errors
/// An endpoint or address the engine refuses, or a file that can't be written.
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

/// Opens the endpoints `file` names, if it says to. A setting the engine refuses, or a
/// port that can't be bound, is logged, and the app runs without them. The endpoints
/// stay open while the handle lives.
pub fn start(engine: &Engine, file: &Path) -> Option<PtzControllers> {
    let setup = match read(file).setup() {
        Ok(Some(setup)) => setup,
        Ok(None) => return None,
        Err(error) => {
            tracing::error!(%error, "the PTZ controller setting names an endpoint JStream refuses; it stays off");
            return None;
        }
    };
    if setup.controllers().is_empty() {
        return None;
    }
    match engine.start_ptz_controllers(&setup) {
        Ok(running) => {
            let endpoints: Vec<String> = setup
                .controllers()
                .iter()
                .map(ToString::to_string)
                .collect();
            tracing::info!(
                local = ?running.local_addrs(),
                endpoints = ?endpoints,
                allow = ?setup.allow(),
                "accepting PTZ controllers"
            );
            Some(running)
        }
        Err(error) => {
            tracing::error!(%error, "could not open the PTZ controller endpoints");
            None
        }
    }
}

/// `set_ptz_controllers`: saves the setting from the app's own window. Answers `null`
/// when it was saved, or why not, for the page to show.
#[cfg(webview)]
#[tauri::command]
pub(crate) fn set_ptz_controllers(
    file: tauri::State<'_, File>,
    setting: Setting,
) -> Option<String> {
    match write(&file.0, &setting) {
        Ok(()) => {
            tracing::info!(
                on = setting.on,
                endpoints = setting.endpoints.len(),
                allowed = setting.allow.len(),
                "PTZ controller setting saved; it applies when the app restarts"
            );
            None
        }
        Err(error) => {
            tracing::warn!(%error, "could not save the PTZ controller setting");
            Some(error)
        }
    }
}

/// The setting's file, the state `set_ptz_controllers` writes.
pub struct File(pub PathBuf);

#[cfg(test)]
mod tests {
    use super::{Endpoint, Setting, read, setting_file, start, write};

    fn folder(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jstream-desktop-ptz-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp folder");
        dir
    }

    fn endpoint(address: &str, transport: &str, target: &str) -> Endpoint {
        Endpoint {
            address: address.into(),
            transport: transport.into(),
            visca: 1,
            target: target.into(),
        }
    }

    /// ADR-0123 §5: off until the person turns it on; on, the endpoints open and answer
    /// only the allowed addresses.
    #[test]
    fn the_setting_is_off_until_the_person_turns_it_on() {
        let dir = folder("round-trip");
        let file = setting_file(&dir);
        assert_eq!(read(&file), Setting::default(), "a missing file is off");
        assert_eq!(read(&file).setup(), Ok(None));
        let engine = jstream_core::Engine::new();
        assert!(start(&engine, &file).is_none(), "off opens nothing");
        let on = Setting {
            on: true,
            endpoints: vec![endpoint("127.0.0.1:0", "visca_ip", "program")],
            allow: vec!["127.0.0.1".into()],
        };
        write(&file, &on).expect("saved");
        assert_eq!(read(&file), on);
        let running = start(&engine, &file).expect("opened");
        assert_eq!(running.local_addrs().len(), 1);
        drop(running);
        // Off keeps the lists, and opens nothing.
        let off = Setting { on: false, ..on };
        write(&file, &off).expect("saved");
        assert_eq!(read(&file).setup(), Ok(None));
        assert_eq!(read(&file).endpoints.len(), 1);
        assert!(start(&engine, &file).is_none());
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
    fn a_refused_endpoint_isnt_saved_and_a_malformed_file_is_off() {
        let dir = folder("refused");
        let file = setting_file(&dir);
        let lan = || vec!["192.168.1.50".to_owned()];
        for (endpoints, allow) in [
            // Every interface, and outside the LAN.
            (
                vec![endpoint("0.0.0.0:52381", "visca_ip", "program")],
                lan(),
            ),
            (
                vec![endpoint("8.8.8.8:52381", "visca_ip", "program")],
                lan(),
            ),
            (
                vec![endpoint("not an address", "visca_ip", "program")],
                lan(),
            ),
            (
                vec![endpoint("192.168.1.20:52381", "serial", "program")],
                lan(),
            ),
            (vec![endpoint("192.168.1.20:52381", "udp", "scene:")], lan()),
            (vec![endpoint("192.168.1.20:52381", "udp", "studio")], lan()),
            // No allowed address, or one outside the LAN.
            (
                vec![endpoint("192.168.1.20:52381", "tcp", "preview")],
                vec![],
            ),
            (
                vec![endpoint("192.168.1.20:52381", "tcp", "preview")],
                vec!["8.8.8.8".to_owned()],
            ),
            // Two endpoints on one address, port and kind of socket.
            (
                vec![
                    endpoint("192.168.1.20:52381", "udp", "program"),
                    endpoint("192.168.1.20:52381", "visca_ip", "preview"),
                ],
                lan(),
            ),
        ] {
            let mut setting = Setting {
                on: true,
                endpoints,
                allow,
            };
            assert!(write(&file, &setting).is_err(), "{setting:?}");
            setting.on = false;
            assert!(write(&file, &setting).is_err(), "off too: {setting:?}");
        }
        let mut bad_visca = Setting {
            on: true,
            endpoints: vec![endpoint("192.168.1.20:52381", "udp", "scene:scn_a")],
            allow: lan(),
        };
        bad_visca.endpoints[0].visca = 8;
        assert!(write(&file, &bad_visca).is_err());
        assert!(!file.exists(), "nothing was saved");
        for text in ["not json", "[true]", r#"{"on":"yes"}"#] {
            std::fs::write(&file, text).expect("written");
            assert_eq!(read(&file), Setting::default(), "{text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
