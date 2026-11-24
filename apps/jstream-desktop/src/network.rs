//! "Allow devices on my network" ([ADR-0104](../../../docs/adr/0104-network-listener-over-tls.md)
//! decision 1): the desktop app's switch for the network listener, the daemon's `--lan`.
//!
//! - **The setting** is `network.json` in the data folder, `{"allow": true}`, user-only.
//!   Only the person changes it, from the app's own window: the page calls
//!   `set_network` through `allowNetwork` on `__JSTREAM_DESKTOP__` (`bootstrap.rs`), a
//!   Tauri command that no engine client (an agent, a plugin or a paired device) can
//!   reach. A missing or unreadable file means off. It applies at once: the command
//!   opens or closes the network listener on the running host (ADR-0234), and answers
//!   why it couldn't open, if it couldn't (most likely another program, such as a daemon
//!   with `--lan`, has the port). Closing it ends the sessions of the devices connected
//!   through it; they stay paired.
//! - **Announcing.** `{"announce": false}` beside it stops the host announcing the
//!   network listener on mDNS as `_jstream._tcp.local` (ADR-0233): "Let devices find
//!   JStream on my network", on unless turned off. The page saves it through
//!   `setAnnounce` and the `set_network_announce` command; it too applies at once.
//!   Each save keeps the file's other key.
//! - **On**, the host opens the network listener on every interface on
//!   [`NETWORK_PORT`], the daemon's default, with the certificate beside the token file
//!   (`jstream_server::identity`, made the first time, never renewed silently), and
//!   pairs devices while it is open, keeping them beside it too, as the daemon does.
//!
//! Docs: apps/jstream-desktop/README.md#design

use anyhow::Context as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The setting's file in the data folder.
pub const NETWORK_FILE: &str = "network.json";

/// The port the network listener uses, the daemon's default (`--lan-port`), so a device
/// paired with either finds the host at the same address.
pub const NETWORK_PORT: u16 = 4456;

/// The setting's file in `data`.
pub fn setting_file(data: &Path) -> PathBuf {
    data.join(NETWORK_FILE)
}

/// Whether the person allowed devices on the network. A missing file is off; one that
/// can't be read or parsed is off too, and logged.
pub fn allowed(file: &Path) -> bool {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(error) => {
            tracing::warn!(file = %file.display(), %error, "could not read the network setting; network access stays off");
            return false;
        }
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) => value.get("allow").and_then(serde_json::Value::as_bool) == Some(true),
        Err(error) => {
            tracing::warn!(file = %file.display(), %error, "the network setting is not JSON; network access stays off");
            false
        }
    }
}

/// Whether the host announces the network listener on mDNS (ADR-0233): on unless the
/// file says `"announce": false`. A missing or unreadable file is on, since the listener
/// it announces is off then anyway.
pub fn announced(file: &Path) -> bool {
    saved(file)
        .get("announce")
        .and_then(serde_json::Value::as_bool)
        != Some(false)
}

/// The file's keys, or none when it is missing or isn't a JSON object.
fn saved(file: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| match value {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

/// Saves `key` as `on`, user-only, keeping the file's other setting, and replacing the
/// file whole.
fn save(file: &Path, key: &str, on: bool) -> std::io::Result<()> {
    let mut map = saved(file);
    map.retain(|name, value| (name == "allow" || name == "announce") && value.is_boolean());
    map.insert(key.to_owned(), serde_json::Value::Bool(on));
    let text = serde_json::Value::Object(map).to_string();
    let temporary = file.with_extension("json.tmp");
    write_private(&temporary, text.as_bytes())?;
    std::fs::rename(&temporary, file)
}

/// Saves "Allow devices on my network".
///
/// # Errors
/// When the file can't be written.
pub fn set_allowed(file: &Path, allow: bool) -> std::io::Result<()> {
    save(file, "allow", allow)
}

/// Saves "Let devices find JStream on my network" (ADR-0233).
///
/// # Errors
/// When the file can't be written.
pub fn set_announced(file: &Path, announce: bool) -> std::io::Result<()> {
    save(file, "announce", announce)
}

/// Writes `bytes` to a new file only this user can read (on Unix; on Windows the data
/// folder under the profile is already the user's).
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut out = options.open(path)?;
    out.write_all(bytes)?;
    out.sync_all()
}

/// The network listener's configuration: every interface on [`NETWORK_PORT`], with
/// the certificate kept beside `token_file` (made the first time), announced on mDNS
/// when `announce` (ADR-0233). A certificate that
/// doesn't name all of today's names is kept and logged, as the daemon keeps it.
///
/// # Errors
/// When the certificate can't be read, made or saved.
pub fn config(token_file: &Path, announce: bool) -> anyhow::Result<jstream_server::NetworkConfig> {
    use jstream_server::identity::{Identity, Made, identity_file, machine_names};
    let ip = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    let names = machine_names(ip);
    let file = identity_file(token_file);
    let (identity, made) = Identity::load_or_create(&file, &names)
        .with_context(|| format!("could not use the network certificate `{}`", file.display()))?;
    if made == Made::Created {
        tracing::info!(file = %file.display(), "network certificate created (user-only)");
    }
    let missing = identity.missing(&names);
    if !missing.is_empty() {
        tracing::warn!(
            missing = %missing.join(", "),
            "the network certificate doesn't name all of this machine's names; devices can \
             still connect, with a name warning"
        );
    }
    Ok(jstream_server::NetworkConfig {
        bind: SocketAddr::new(ip, NETWORK_PORT),
        identity: Arc::new(identity),
        announce,
    })
}

/// `set_network`: saves the setting from the app's own window and applies it at once.
/// Answers `null`, or why it wasn't saved or the listener couldn't open.
#[cfg(webview)]
#[tauri::command]
pub(crate) async fn set_network(
    setting: tauri::State<'_, Setting>,
    allow: bool,
) -> Result<Option<String>, ()> {
    Ok(setting.allow(allow).await)
}

/// `set_network_announce`: saves "Let devices find JStream on my network" from the
/// app's own window (ADR-0233) and applies it at once. Answers whether it was saved.
#[cfg(webview)]
#[tauri::command]
pub(crate) async fn set_network_announce(
    setting: tauri::State<'_, Setting>,
    announce: bool,
) -> Result<bool, ()> {
    Ok(setting.announce(announce).await)
}

/// The setting's file, the state `set_network` and `set_network_announce` write, and
/// the running host's listener they switch.
pub struct Setting {
    /// The setting's file.
    pub file: PathBuf,
    /// The running host's network listener, when the engine started.
    pub live: Option<Live>,
}

/// A running host's network listener, as the setting switches it.
pub struct Live {
    /// The host's switch for it (`Host::network_switch`).
    pub switch: jstream_server::NetworkSwitch,
    /// The host's runtime, where the listener runs.
    pub runtime: tokio::runtime::Handle,
    /// The token file the certificate is kept beside (`config`).
    pub token_file: PathBuf,
}

impl Setting {
    /// Saves "Allow devices on my network" as `allow`, then opens or closes the
    /// listener. Answers `None`, or why it wasn't saved or couldn't open.
    pub async fn allow(&self, allow: bool) -> Option<String> {
        if let Err(error) = set_allowed(&self.file, allow) {
            tracing::warn!(%error, "could not save the network setting");
            return Some(format!("could not save the setting: {error}"));
        }
        tracing::info!(allow, "network setting saved");
        let live = self.live.as_ref()?;
        let switch = live.switch.clone();
        if !allow {
            let _ = live
                .runtime
                .spawn(async move { switch.close().await })
                .await;
            return None;
        }
        let config = match config(&live.token_file, announced(&self.file)) {
            Ok(config) => config,
            Err(error) => {
                tracing::error!(error = format!("{error:#}"), "network access stays off");
                return Some(format!("{error:#}"));
            }
        };
        let opened = live
            .runtime
            .spawn(async move { switch.open(config).await })
            .await;
        match opened {
            Ok(Ok(info)) => {
                tracing::info!(
                    urls = %info.urls.join(", "),
                    fingerprint = %info.fingerprint,
                    "devices on the network may pair (Allow devices on my network)"
                );
                None
            }
            Ok(Err(error)) => {
                tracing::error!(%error, "could not open the network listener");
                Some(format!(
                    "{error}. Another program, such as JStream's daemon, may be using port \
                     {NETWORK_PORT}."
                ))
            }
            Err(error) => Some(format!("could not open the network listener: {error}")),
        }
    }

    /// Saves "Let devices find JStream on my network" as `announce`, then starts or
    /// stops announcing the open listener. Answers whether it was saved.
    pub async fn announce(&self, announce: bool) -> bool {
        if let Err(error) = set_announced(&self.file, announce) {
            tracing::warn!(%error, "could not save the network announcement setting");
            return false;
        }
        tracing::info!(announce, "network announcement setting saved");
        if let Some(live) = &self.live {
            let switch = live.switch.clone();
            let _ = live
                .runtime
                .spawn(async move { switch.set_announce(announce).await })
                .await;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{allowed, announced, set_allowed, set_announced, setting_file};

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jstream-desktop-network-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp folder");
        dir
    }

    /// Off until saved on, and a saved off reads off again.
    #[test]
    fn the_setting_is_off_until_the_person_turns_it_on() {
        let dir = folder("round-trip");
        let file = setting_file(&dir);
        assert!(!allowed(&file), "a missing file is off");
        set_allowed(&file, true).expect("saved");
        assert!(allowed(&file));
        set_allowed(&file, false).expect("saved");
        assert!(!allowed(&file));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            set_allowed(&file, true).expect("saved");
            let mode = std::fs::metadata(&file)
                .expect("there")
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "user-only");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Anything but `{"allow": true}` is off.
    #[test]
    fn a_malformed_setting_is_off() {
        let dir = folder("malformed");
        let file = setting_file(&dir);
        for text in [
            "not json",
            "{}",
            r#"{"allow":"yes"}"#,
            "[true]",
            r#"{"allow":1}"#,
        ] {
            std::fs::write(&file, text).expect("written");
            assert!(!allowed(&file), "{text}");
        }
        std::fs::write(&file, r#"{"allow":true}"#).expect("written");
        assert!(allowed(&file));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Announcing is on until turned off, and each switch keeps the other (ADR-0233).
    #[test]
    fn announcing_is_on_until_turned_off_and_keeps_allow() {
        let dir = folder("announce");
        let file = setting_file(&dir);
        assert!(announced(&file), "a missing file announces");
        set_allowed(&file, true).expect("saved");
        assert!(announced(&file));
        set_announced(&file, false).expect("saved");
        assert!(!announced(&file));
        assert!(allowed(&file), "turning announcing off kept allow");
        set_allowed(&file, false).expect("saved");
        assert!(!announced(&file), "turning allow off kept announce");
        set_announced(&file, true).expect("saved");
        assert!(announced(&file) && !allowed(&file));
        std::fs::write(&file, "not json").expect("written");
        assert!(announced(&file));
        set_announced(&file, false).expect("saved over a broken file");
        assert!(!announced(&file) && !allowed(&file));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
