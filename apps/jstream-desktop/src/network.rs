//! "Allow devices on my network" ([ADR-0104](../../../docs/adr/0104-network-listener-over-tls.md)
//! decision 1): the desktop app's switch for the network listener, the daemon's `--lan`.
//!
//! - **The setting** is `network.json` in the data folder, `{"allow": true}`, user-only.
//!   Only the person changes it, from the app's own window: the page calls
//!   `set_network` through `allowNetwork` on `__JSTREAM_DESKTOP__` (`bootstrap.rs`), a
//!   Tauri command that no engine client (an agent, a plugin or a paired device) can
//!   reach. A missing or unreadable file means off. It takes effect when the app next
//!   starts.
//! - **On**, the host opens the network listener on every interface on
//!   [`NETWORK_PORT`], the daemon's default, with the certificate beside the token file
//!   (`jstream_server::identity`, made the first time, never renewed silently), and
//!   pairs devices, keeping them beside it too, as the daemon does.
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

/// Saves the setting, user-only, replacing the file whole.
///
/// # Errors
/// When the file can't be written.
pub fn set_allowed(file: &Path, allow: bool) -> std::io::Result<()> {
    let text = serde_json::json!({ "allow": allow }).to_string();
    let temporary = file.with_extension("json.tmp");
    write_private(&temporary, text.as_bytes())?;
    std::fs::rename(&temporary, file)
}

/// Writes `bytes` to a new file only this user can read (on Unix; on Windows the data
/// folder under the profile is already the user's).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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
/// the certificate kept beside `token_file` (made the first time). A certificate that
/// doesn't name all of today's names is kept and logged, as the daemon keeps it.
///
/// # Errors
/// When the certificate can't be read, made or saved.
pub fn config(token_file: &Path) -> anyhow::Result<jstream_server::NetworkConfig> {
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
    })
}

/// `set_network`: saves the setting from the app's own window. Answers whether it was
/// saved.
#[cfg(webview)]
#[tauri::command]
pub(crate) fn set_network(setting: tauri::State<'_, Setting>, allow: bool) -> bool {
    match set_allowed(&setting.0, allow) {
        Ok(()) => {
            tracing::info!(
                allow,
                "network setting saved; it applies when the app restarts"
            );
            true
        }
        Err(error) => {
            tracing::warn!(%error, "could not save the network setting");
            false
        }
    }
}

/// The setting's file, the state `set_network` writes.
pub struct Setting(pub PathBuf);

#[cfg(test)]
mod tests {
    use super::{allowed, set_allowed, setting_file};

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
}
