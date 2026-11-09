//! "Receive from JTunnel senders" ([ADR-0123](../../../docs/adr/0123-lan-inputs-jtunnel-and-visca.md)
//! decisions 1 and 3): the desktop app's switch for the JTunnel receiver, and the
//! machine's accepted senders, as the daemon's `--jtunnel-accept` gives them.
//!
//! - **The setting** is `jtunnel.json` in the data folder, `{"on": true, "accepted":
//!   [{"name": "Bitwig", "address": "192.168.1.5:9000"}]}`, user-only. Only the person
//!   changes it, from the app's own window: the page calls `set_jtunnel` through
//!   `setJtunnel` on `__JSTREAM_DESKTOP__` (`bootstrap.rs`), a Tauri command that no
//!   engine client (an agent, a plugin or a paired device) can reach. Per machine, never
//!   in the collection, so opening a collection never turns it on. A missing or
//!   unreadable file means off. It takes effect when the app next starts.
//! - **On**, the app starts the engine's JTunnel receiver (`Engine::start_jtunnel`)
//!   with the accepted senders, after the platform's backends, whose media clock stamps
//!   the arrivals. With none accepted it still runs, so it can find senders on the LAN
//!   (`jtunnel.senders`) for the person to accept. Off, no JTunnel socket is open.
//!
//! Docs: apps/jstream-desktop/README.md#design

use jstream_core::{AcceptedSender, Engine, JtunnelSetup};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The setting's file in the data folder.
pub const JTUNNEL_FILE: &str = "jtunnel.json";

/// The setting's file in `data`.
pub fn setting_file(data: &Path) -> PathBuf {
    data.join(JTUNNEL_FILE)
}

/// One accepted sender as the file and the page hold it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Its tunnel name.
    pub name: String,
    /// The address and port it sends from, as `192.168.1.5:9000`.
    pub address: String,
}

/// The setting: whether receiving is on, and the senders this machine accepts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Setting {
    /// "Receive from JTunnel senders".
    pub on: bool,
    /// The accepted senders, in the order the person added them.
    pub accepted: Vec<Entry>,
}

impl Setting {
    /// The receiver's setup: `None` while off. On with no sender, it receives from
    /// nobody and only finds senders on the LAN.
    ///
    /// # Errors
    /// A sender the engine refuses ([`AcceptedSender::new`], [`JtunnelSetup::new`]),
    /// named in the message.
    pub fn setup(&self) -> Result<Option<JtunnelSetup>, String> {
        if !self.on {
            return Ok(None);
        }
        let accepted = self
            .accepted
            .iter()
            .map(|entry| {
                let addr = entry.address.trim().parse().map_err(|err| {
                    format!("`{}` is not an address and port: {err}", entry.address)
                })?;
                AcceptedSender::new(&entry.name, addr)
            })
            .collect::<Result<Vec<_>, String>>()?;
        JtunnelSetup::new(accepted).map(Some)
    }
}

/// The setting in `file`. A missing file is off; one that can't be read or parsed is
/// off too, and logged.
pub fn read(file: &Path) -> Setting {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Setting::default(),
        Err(error) => {
            tracing::warn!(file = %file.display(), %error, "could not read the JTunnel setting; receiving stays off");
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
            tracing::warn!(file = %file.display(), %error, "the JTunnel setting is not valid; receiving stays off");
            Setting::default()
        }
    }
}

/// Saves `setting`, user-only, replacing the file whole, once the engine would take it.
///
/// # Errors
/// A sender the engine refuses, or a file that can't be written.
pub fn write(file: &Path, setting: &Setting) -> Result<(), String> {
    setting.setup()?;
    let text = serde_json::to_string(setting).map_err(|err| err.to_string())?;
    let temporary = file.with_extension("json.tmp");
    // A leftover from a write that didn't finish keeps its own mode when opened again,
    // so it goes first and the new one is created user-only (#1446 review N1).
    match std::fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.to_string()),
    }
    crate::network::write_private(&temporary, text.as_bytes()).map_err(|err| err.to_string())?;
    std::fs::rename(&temporary, file).map_err(|err| err.to_string())
}

/// Starts the engine's JTunnel receiver as `file` says, if it says to. A setting the
/// engine refuses, or a socket that can't be bound, is logged, and the app runs without
/// it. Returns whether the receiver started.
pub fn start(engine: &Engine, file: &Path) -> bool {
    let setting = read(file);
    let setup = match setting.setup() {
        Ok(Some(setup)) => setup,
        Ok(None) => return false,
        Err(error) => {
            tracing::error!(%error, "the JTunnel setting names a sender JStream refuses; receiving stays off");
            return false;
        }
    };
    match engine.start_jtunnel(&setup) {
        Ok(local) => {
            let senders: Vec<String> = setting
                .accepted
                .iter()
                .map(|entry| format!("{}@{}", entry.name, entry.address))
                .collect();
            tracing::info!(%local, senders = ?senders, "receiving from JTunnel senders");
            true
        }
        Err(error) => {
            tracing::error!(%error, "could not start receiving from JTunnel senders");
            false
        }
    }
}

/// `set_jtunnel`: saves the setting from the app's own window. Answers `null` when it
/// was saved, or why not, for the page to show.
#[cfg(webview)]
#[tauri::command]
pub(crate) fn set_jtunnel(file: tauri::State<'_, File>, setting: Setting) -> Option<String> {
    match write(&file.0, &setting) {
        Ok(()) => {
            tracing::info!(
                on = setting.on,
                senders = setting.accepted.len(),
                "JTunnel setting saved; it applies when the app restarts"
            );
            None
        }
        Err(error) => {
            tracing::warn!(%error, "could not save the JTunnel setting");
            Some(error)
        }
    }
}

/// The setting's file, the state `set_jtunnel` writes.
pub struct File(pub PathBuf);

#[cfg(test)]
mod tests {
    use super::{Entry, Setting, read, setting_file, write};

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jstream-desktop-jtunnel-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp folder");
        dir
    }

    fn entry(name: &str, address: &str) -> Entry {
        Entry {
            name: name.into(),
            address: address.into(),
        }
    }

    /// ADR-0123 §1: off until the person turns it on, and on with no sender opens nothing.
    #[test]
    fn the_setting_is_off_until_the_person_turns_it_on() {
        let dir = folder("round-trip");
        let file = setting_file(&dir);
        assert_eq!(read(&file), Setting::default(), "a missing file is off");
        assert_eq!(read(&file).setup(), Ok(None));
        let on = Setting {
            on: true,
            accepted: vec![entry("Bitwig", "192.168.1.5:9000")],
        };
        write(&file, &on).expect("saved");
        assert_eq!(read(&file), on);
        let setup = read(&file).setup().expect("valid").expect("a setup");
        assert_eq!(setup.accepted[0].name(), "Bitwig");
        // Off keeps the list, and opens nothing.
        let off = Setting { on: false, ..on };
        write(&file, &off).expect("saved");
        assert_eq!(read(&file).setup(), Ok(None));
        assert_eq!(read(&file).accepted.len(), 1);
        // On with no sender still runs the receiver, to find senders to accept.
        let empty = Setting {
            on: true,
            accepted: Vec::new(),
        };
        assert!(
            empty
                .setup()
                .expect("valid")
                .is_some_and(|s| s.accepted.is_empty())
        );
        // A leftover temporary file of another mode is replaced, not reused.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let temporary = file.with_extension("json.tmp");
            std::fs::write(&temporary, b"{}").expect("written");
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o644))
                .expect("mode");
            write(&file, &empty).expect("saved");
            let mode = std::fs::metadata(&file)
                .expect("there")
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "user-only after a leftover");
        }
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

    /// A sender the engine refuses is never saved, and a malformed file is off.
    #[test]
    fn a_refused_sender_isnt_saved_and_a_malformed_file_is_off() {
        let dir = folder("refused");
        let file = setting_file(&dir);
        for bad in [
            vec![entry("Bitwig", "8.8.8.8:9000")],
            vec![entry("Bitwig", "not an address")],
            vec![
                entry("A", "192.168.1.5:9000"),
                entry("A", "192.168.1.6:9000"),
            ],
            // Two senders at one address and port, a padded name, IPv6: the engine's
            // own checks (#1444).
            vec![
                entry("A", "192.168.1.5:9000"),
                entry("B", "192.168.1.5:9000"),
            ],
            vec![entry(" Bitwig", "192.168.1.5:9000")],
            vec![entry("Bitwig", "[::1]:9000")],
        ] {
            let setting = Setting {
                on: true,
                accepted: bad,
            };
            assert!(write(&file, &setting).is_err(), "{setting:?}");
        }
        assert!(!file.exists(), "nothing was saved");
        for text in ["not json", "[true]", r#"{"on":"yes"}"#] {
            std::fs::write(&file, text).expect("written");
            assert_eq!(read(&file), Setting::default(), "{text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
