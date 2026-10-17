//! Paired devices and the pairing code (ADR-0044).
//!
//! - **The pairing code.** `pairing.start` draws one: 40 random bits written as 8
//!   characters of Crockford's base32 alphabet, shown `XXXX-XXXX`. One is open at a time,
//!   and a new one replaces it. It is open for [`CODE_LIFETIME`], is used once, and is
//!   void after [`MAX_WRONG_ATTEMPTS`] wrong attempts. Only its BLAKE3 digest is kept, and
//!   a presented code is compared digest to digest, as the control token is.
//! - **Device tokens.** A right code gets a new 256-bit token, written as 64 hex digits
//!   like the control token. The server keeps only its BLAKE3 digest, with the device's
//!   ID, its name, when it paired and when its token was last presented. A token unused
//!   for [`LAPSE`] lapses and its device is dropped.
//! - **The device file.** With `ServerConfig::device_file` the devices are kept in that
//!   file, user-only on Unix (mode 0600; on Windows it gets its directory's ACL, like the
//!   token file), rewritten whole through a temporary file on every change. It holds
//!   digests, never tokens. Without it they live in memory and are gone when the server
//!   stops.
//! - **Revocation.** Each open connection a device token admitted watches its device;
//!   revoking it, or its lapse, signals the watch, and the connection closes.
//!
//! Nothing here logs a code or a token (R-SEC-001): the log lines carry device IDs and
//! names only.

use crate::error::ServerError;
use jstream_protocol::{DeviceId, DeviceInfo};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

/// How long a pairing code stays open.
pub(crate) const CODE_LIFETIME: Duration = Duration::from_secs(120);

/// How many wrong codes void the open one.
pub(crate) const MAX_WRONG_ATTEMPTS: u32 = 5;

/// How long a device token may go unused before it lapses: 30 days, in seconds.
pub(crate) const LAPSE: u64 = 30 * 24 * 3600;

/// How stale a device's stored `last_used_at` may get before a use rewrites the file, in
/// seconds. Recording every use would rewrite it on every request.
const TOUCH_EVERY: u64 = 3600;

/// The longest device name accepted, in characters.
pub(crate) const MAX_NAME_CHARS: usize = 64;

/// Random bytes in a device token: 256 bits.
const TOKEN_BYTES: usize = 32;

/// Crockford's base32 alphabet: digits and letters without I, L, O and U.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The device file's format version.
const FILE_VERSION: u64 = 1;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The current time, both ways the store needs it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Now {
    /// Seconds since the Unix epoch, for what is stored.
    pub(crate) unix: u64,
    /// The monotonic clock, for the pairing code's deadline.
    pub(crate) instant: Instant,
}

impl Now {
    pub(crate) fn current() -> Now {
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        Now {
            unix,
            instant: Instant::now(),
        }
    }
}

/// One paired device.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Device {
    name: String,
    digest: blake3::Hash,
    paired_at: u64,
    last_used_at: u64,
}

impl Device {
    fn lapsed(&self, now: u64) -> bool {
        now.saturating_sub(self.last_used_at) >= LAPSE
    }
}

/// The open pairing code.
struct Pairing {
    /// The digest of the code, normalized (8 symbols, no dash).
    digest: blake3::Hash,
    expires: Instant,
    wrong: u32,
}

/// Why `pair` refused.
#[derive(Debug)]
pub(crate) enum PairError {
    /// No code is open: none was started, it expired, was used, or was voided.
    NoCode,
    /// The code is wrong. `voided` when this attempt used up the last one.
    Wrong { voided: bool },
    /// The OS's random source failed.
    Random(getrandom::Error),
    /// The device file could not be written; nothing was paired.
    Store(io::Error),
}

#[derive(Default)]
struct State {
    devices: BTreeMap<DeviceId, Device>,
    pairing: Option<Pairing>,
    /// One watch per device that has open connections; signalled when it is revoked or
    /// lapses.
    watches: HashMap<DeviceId, watch::Sender<bool>>,
}

impl State {
    /// Drops the devices whose tokens lapsed, closing their connections. Whether any was.
    fn drop_lapsed(&mut self, now: u64) -> bool {
        let lapsed: Vec<DeviceId> = self
            .devices
            .iter()
            .filter(|(_, device)| device.lapsed(now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &lapsed {
            self.devices.remove(id);
            self.signal(id);
            tracing::info!(device_id = %id, "device dropped: its token was unused for 30 days");
        }
        !lapsed.is_empty()
    }

    fn signal(&mut self, id: &DeviceId) {
        if let Some(watch) = self.watches.remove(id) {
            watch.send_replace(true);
        }
    }
}

/// The paired devices and the open pairing code, shared by every connection.
pub(crate) struct Devices {
    file: Option<PathBuf>,
    state: Mutex<State>,
}

impl Devices {
    /// The devices kept in `file`, read now (a missing file means none), or kept in
    /// memory only without one.
    pub(crate) fn load(file: Option<PathBuf>) -> Result<Devices, ServerError> {
        let mut state = State::default();
        if let Some(path) = &file {
            state.devices = read_file(path).map_err(|reason| ServerError::DeviceFile {
                path: path.clone(),
                reason,
            })?;
        }
        Ok(Devices {
            file,
            state: Mutex::new(state),
        })
    }

    /// Opens a new pairing code, replacing any open one, and returns it as `XXXX-XXXX`.
    pub(crate) fn start_pairing(&self, now: Now) -> Result<String, getrandom::Error> {
        let mut bytes = [0u8; 5];
        getrandom::fill(&mut bytes)?;
        let bits = bytes.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b));
        let symbols: String = (0..8)
            .rev()
            .map(|i| char::from(CROCKFORD[((bits >> (i * 5)) & 31) as usize]))
            .collect();
        lock(&self.state).pairing = Some(Pairing {
            digest: blake3::hash(symbols.as_bytes()),
            expires: now.instant + CODE_LIFETIME,
            wrong: 0,
        });
        Ok(format!("{}-{}", &symbols[..4], &symbols[4..]))
    }

    /// Whether a pairing code is open now.
    pub(crate) fn pairing_open(&self, now: Now) -> bool {
        let mut state = lock(&self.state);
        open_pairing(&mut state, now).is_some()
    }

    /// Pairs a device named `name` (already checked) if `code` is the open code, and
    /// returns its ID and its new token. The code is used up by a right attempt and
    /// voided by the last wrong one.
    pub(crate) fn pair(
        &self,
        code: &str,
        name: &str,
        now: Now,
    ) -> Result<(DeviceId, String), PairError> {
        let mut state = lock(&self.state);
        let pairing = open_pairing(&mut state, now).ok_or(PairError::NoCode)?;
        let right = normalize(code).is_some_and(|c| blake3::hash(c.as_bytes()) == pairing.digest);
        if !right {
            pairing.wrong += 1;
            let voided = pairing.wrong >= MAX_WRONG_ATTEMPTS;
            if voided {
                state.pairing = None;
                tracing::warn!("pairing code voided after 5 wrong attempts");
            }
            return Err(PairError::Wrong { voided });
        }
        state.pairing = None;
        let mut bytes = [0u8; TOKEN_BYTES + 8];
        getrandom::fill(&mut bytes).map_err(PairError::Random)?;
        let token: String = bytes[..TOKEN_BYTES]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let id: String = bytes[TOKEN_BYTES..]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let id = DeviceId::new(format!("dev_{id}"));
        state.drop_lapsed(now.unix);
        state.devices.insert(
            id.clone(),
            Device {
                name: name.to_owned(),
                digest: blake3::hash(token.as_bytes()),
                paired_at: now.unix,
                last_used_at: now.unix,
            },
        );
        if let Err(error) = self.save(&state) {
            state.devices.remove(&id);
            return Err(PairError::Store(error));
        }
        tracing::info!(device_id = %id, name, "device paired");
        Ok((id, token))
    }

    /// The device whose token `presented` is, with its name, marking it used now. The
    /// presented value is hashed and compared digest to digest with every device's, so
    /// the compare reveals nothing about any token.
    pub(crate) fn admit(&self, presented: &[u8], now: u64) -> Option<(DeviceId, String)> {
        let digest = blake3::hash(presented);
        let mut state = lock(&self.state);
        let mut changed = state.drop_lapsed(now);
        let mut found = None;
        for (id, device) in &mut state.devices {
            if device.digest == digest {
                if now.saturating_sub(device.last_used_at) >= TOUCH_EVERY {
                    device.last_used_at = now;
                    changed = true;
                }
                found = Some((id.clone(), device.name.clone()));
            }
        }
        if changed && let Err(error) = self.save(&state) {
            tracing::warn!(error = %error, "could not update the device file");
        }
        found
    }

    /// Whether `id` is paired (and not lapsed).
    pub(crate) fn contains(&self, id: &DeviceId, now: u64) -> bool {
        lock(&self.state)
            .devices
            .get(id)
            .is_some_and(|device| !device.lapsed(now))
    }

    /// A watch that turns `true` (or closes) when `id` is revoked or lapses; `None` if it
    /// is not paired.
    pub(crate) fn watch(&self, id: &DeviceId) -> Option<watch::Receiver<bool>> {
        let mut state = lock(&self.state);
        if !state.devices.contains_key(id) {
            return None;
        }
        let watch = state
            .watches
            .entry(id.clone())
            .or_insert_with(|| watch::channel(false).0);
        Some(watch.subscribe())
    }

    /// The paired devices, ordered by ID.
    pub(crate) fn list(&self, now: u64) -> Vec<DeviceInfo> {
        let mut state = lock(&self.state);
        if state.drop_lapsed(now)
            && let Err(error) = self.save(&state)
        {
            tracing::warn!(error = %error, "could not update the device file");
        }
        state
            .devices
            .iter()
            .map(|(id, device)| DeviceInfo {
                device_id: id.clone(),
                name: device.name.clone(),
                paired_at: device.paired_at,
                last_used_at: device.last_used_at,
            })
            .collect()
    }

    /// Revokes `id`: its token stops working and its connections are told to close.
    /// `Ok(false)` if it was not paired. The device is revoked in memory even if the file
    /// can't be written, which is the error.
    pub(crate) fn revoke(&self, id: &DeviceId) -> io::Result<bool> {
        let mut state = lock(&self.state);
        if state.devices.remove(id).is_none() {
            return Ok(false);
        }
        state.signal(id);
        tracing::info!(device_id = %id, "device revoked");
        self.save(&state).map(|()| true)
    }

    /// Writes every device to the file, if there is one.
    fn save(&self, state: &State) -> io::Result<()> {
        match &self.file {
            Some(path) => write_file(path, &state.devices),
            None => Ok(()),
        }
    }
}

/// The open pairing code, after dropping one that expired.
fn open_pairing(state: &mut State, now: Now) -> Option<&mut Pairing> {
    if state
        .pairing
        .as_ref()
        .is_some_and(|pairing| now.instant >= pairing.expires)
    {
        state.pairing = None;
    }
    state.pairing.as_mut()
}

/// A typed code as its 8 symbols: dashes and spaces dropped, letters upper-cased, and
/// Crockford's look-alikes read as the digits they stand for (`O` as 0, `I` and `L` as
/// 1). `None` unless that leaves 8 symbols of the alphabet.
fn normalize(code: &str) -> Option<String> {
    let symbols: String = code
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect();
    let valid = symbols.len() == 8 && symbols.bytes().all(|b| CROCKFORD.contains(&b));
    valid.then_some(symbols)
}

/// Checks a device name: trimmed, 1 to [`MAX_NAME_CHARS`] characters, none of them a
/// control character. The name is shown in `device.list` and the audit log.
pub(crate) fn check_name(name: &str) -> Result<&str, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("the device name is empty");
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err("the device name is over 64 characters");
    }
    if name.chars().any(char::is_control) {
        return Err("the device name has a control character");
    }
    Ok(name)
}

/// Reads the device file: a missing one holds no devices. On Unix a file group or others
/// can access is refused, as the token file is.
fn read_file(path: &Path) -> Result<BTreeMap<DeviceId, Device>, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file() {
        return Err("it is not a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "it is accessible to other users (mode {:o}); make it user-only (chmod 600)",
                mode & 0o777
            ));
        }
    }
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    parse_file(&text)
}

fn parse_file(text: &str) -> Result<BTreeMap<DeviceId, Device>, String> {
    let value: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    if value.get("version").and_then(Value::as_u64) != Some(FILE_VERSION) {
        return Err(format!("it is not a version {FILE_VERSION} device file"));
    }
    let entries = value
        .get("devices")
        .and_then(Value::as_array)
        .ok_or("it has no `devices` list")?;
    let mut devices = BTreeMap::new();
    for (n, entry) in entries.iter().enumerate() {
        let text = |key: &str| entry.get(key).and_then(Value::as_str);
        let number = |key: &str| entry.get(key).and_then(Value::as_u64);
        let broken = || format!("device {n} is incomplete or malformed");
        let id = text("device_id").ok_or_else(broken)?;
        let digest = text("digest")
            .and_then(|hex| blake3::Hash::from_hex(hex).ok())
            .ok_or_else(broken)?;
        let device = Device {
            name: text("name").ok_or_else(broken)?.to_owned(),
            digest,
            paired_at: number("paired_at").ok_or_else(broken)?,
            last_used_at: number("last_used_at").ok_or_else(broken)?,
        };
        devices.insert(DeviceId::new(id), device);
    }
    Ok(devices)
}

fn render_file(devices: &BTreeMap<DeviceId, Device>) -> String {
    let entries: Vec<Value> = devices
        .iter()
        .map(|(id, device)| {
            json!({
                "device_id": id.as_str(),
                "name": device.name,
                "digest": device.digest.to_hex().as_str(),
                "paired_at": device.paired_at,
                "last_used_at": device.last_used_at,
            })
        })
        .collect();
    let mut text = json!({"version": FILE_VERSION, "devices": entries}).to_string();
    text.push('\n');
    text
}

/// Writes the device file whole: to `<path>.tmp`, created user-only on Unix, synced, then
/// renamed over `path`, so a crash leaves the old file or the new one.
fn write_file(path: &Path, devices: &BTreeMap<DeviceId, Device>) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    // A temporary file left by a crash is ours to replace.
    match fs::remove_file(&tmp) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options.open(&tmp).and_then(|mut file| {
        file.write_all(render_file(devices).as_bytes())?;
        file.sync_all()
    });
    let renamed = written.and_then(|()| fs::rename(&tmp, path));
    if renamed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    renamed
}

#[cfg(test)]
mod tests {
    use super::{
        CODE_LIFETIME, Devices, LAPSE, MAX_WRONG_ATTEMPTS, Now, PairError, check_name, normalize,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    fn now() -> Now {
        Now::current()
    }

    fn later(now: Now, secs: u64) -> Now {
        Now {
            unix: now.unix + secs,
            instant: now.instant + Duration::from_secs(secs),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jstream-server-devices-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the scratch directory is created");
        dir
    }

    fn memory() -> Devices {
        Devices::load(None).expect("in memory")
    }

    #[test]
    fn codes_are_eight_crockford_symbols_and_typed_loosely() {
        let devices = memory();
        let code = devices.start_pairing(now()).expect("random");
        assert_eq!(code.len(), 9);
        assert_eq!(&code[4..5], "-");
        assert!(
            code.chars()
                .filter(|c| *c != '-')
                .all(|c| "0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(c)),
            "{code}"
        );
        assert_eq!(normalize("7k3m 9qxa").as_deref(), Some("7K3M9QXA"));
        assert_eq!(normalize("OIL0-0000").as_deref(), Some("01100000"));
        assert_eq!(normalize("7K3M-9QXU"), None, "U is not in the alphabet");
        assert_eq!(normalize("7K3M-9QX"), None, "too short");
    }

    #[test]
    fn a_right_code_pairs_once_and_its_token_admits() {
        let devices = memory();
        let t = now();
        assert!(matches!(
            devices.pair("0000-0000", "Tablet", t),
            Err(PairError::NoCode)
        ));
        let code = devices.start_pairing(t).expect("random");
        assert!(devices.pairing_open(t));
        let (id, token) = devices
            .pair(&code.to_lowercase(), "Tablet", t)
            .expect("paired");
        assert_eq!(token.len(), 64);
        assert!(!devices.pairing_open(t), "single-use");
        assert!(matches!(
            devices.pair(&code, "Again", t),
            Err(PairError::NoCode)
        ));
        assert_eq!(
            devices.admit(token.as_bytes(), t.unix),
            Some((id.clone(), "Tablet".to_owned()))
        );
        assert_eq!(devices.admit(b"not-the-token", t.unix), None);
        let listed = devices.list(t.unix);
        assert_eq!(listed.len(), 1);
        assert_eq!(
            (&listed[0].device_id, listed[0].name.as_str()),
            (&id, "Tablet")
        );
    }

    #[test]
    fn a_new_start_replaces_the_open_code() {
        let devices = memory();
        let t = now();
        let first = devices.start_pairing(t).expect("random");
        let second = devices.start_pairing(t).expect("random");
        if first != second {
            assert!(matches!(
                devices.pair(&first, "Old", t),
                Err(PairError::Wrong { voided: false })
            ));
        }
        assert!(devices.pair(&second, "New", t).is_ok());
    }

    #[test]
    fn codes_expire_and_wrong_attempts_void_them() {
        let devices = memory();
        let t = now();
        let code = devices.start_pairing(t).expect("random");
        let expired = later(t, CODE_LIFETIME.as_secs());
        assert!(!devices.pairing_open(expired));
        assert!(matches!(
            devices.pair(&code, "Late", expired),
            Err(PairError::NoCode)
        ));

        let code = devices.start_pairing(t).expect("random");
        // A code that differs from the open one in its last symbol.
        let wrong = if code.ends_with('0') {
            format!("{}1", &code[..8])
        } else {
            format!("{}0", &code[..8])
        };
        for attempt in 1..=MAX_WRONG_ATTEMPTS {
            let voided = attempt == MAX_WRONG_ATTEMPTS;
            assert!(
                matches!(devices.pair(&wrong, "Guess", t), Err(PairError::Wrong { voided: v }) if v == voided),
                "attempt {attempt}"
            );
        }
        assert!(matches!(
            devices.pair(&code, "Right, too late", t),
            Err(PairError::NoCode)
        ));
    }

    #[test]
    fn revoking_and_lapsing_drop_a_device_and_signal_its_connections() {
        let devices = memory();
        let t = now();
        let code = devices.start_pairing(t).expect("random");
        let (id, token) = devices.pair(&code, "Tablet", t).expect("paired");
        let watch = devices.watch(&id).expect("paired");
        assert!(devices.revoke(&id).expect("in memory"));
        assert!(*watch.borrow() || watch.has_changed().is_err());
        assert_eq!(devices.admit(token.as_bytes(), t.unix), None);
        assert!(!devices.revoke(&id).expect("in memory"), "already gone");
        assert!(devices.watch(&id).is_none());

        let code = devices.start_pairing(t).expect("random");
        let (id, token) = devices.pair(&code, "Laptop", t).expect("paired");
        let watch = devices.watch(&id).expect("paired");
        assert!(devices.contains(&id, t.unix + LAPSE - 1));
        assert_eq!(devices.admit(token.as_bytes(), t.unix + LAPSE), None);
        assert!(*watch.borrow() || watch.has_changed().is_err());
        assert!(devices.list(t.unix + LAPSE).is_empty());
    }

    #[test]
    fn use_postpones_the_lapse() {
        let devices = memory();
        let t = now();
        let code = devices.start_pairing(t).expect("random");
        let (id, token) = devices.pair(&code, "Tablet", t).expect("paired");
        let used = t.unix + LAPSE - 10;
        assert!(devices.admit(token.as_bytes(), used).is_some());
        assert!(devices.contains(&id, t.unix + LAPSE + 10));
        assert_eq!(devices.list(t.unix + LAPSE + 10)[0].last_used_at, used);
    }

    #[test]
    fn the_file_keeps_digests_only_and_survives_a_restart() {
        let dir = scratch("file");
        let path = dir.join("token.devices");
        let devices = Devices::load(Some(path.clone())).expect("no file yet");
        let t = now();
        let code = devices.start_pairing(t).expect("random");
        let (id, token) = devices.pair(&code, "Tablet", t).expect("paired");
        let text = fs::read_to_string(&path).expect("written");
        assert!(!text.contains(&token), "{text}");
        assert!(
            text.contains(id.as_str()) && text.contains("Tablet"),
            "{text}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("exists").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "user-only");
        }

        let reloaded = Devices::load(Some(path.clone())).expect("reads");
        assert_eq!(
            reloaded.admit(token.as_bytes(), t.unix),
            Some((id.clone(), "Tablet".to_owned()))
        );
        assert!(reloaded.revoke(&id).expect("written"));
        let reloaded = Devices::load(Some(path.clone())).expect("reads");
        assert!(reloaded.list(t.unix).is_empty());

        fs::write(&path, "not json").expect("written");
        assert!(Devices::load(Some(path.clone())).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_device_file_other_users_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        let path = dir.join("token.devices");
        fs::write(&path, "{\"version\":1,\"devices\":[]}\n").expect("written");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        let error = Devices::load(Some(path.clone()))
            .err()
            .map(|e| e.to_string());
        assert!(
            error.as_deref().is_some_and(|e| e.contains("mode 644")),
            "{error:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_are_short_visible_text() {
        assert_eq!(check_name("  Tablet "), Ok("Tablet"));
        assert!(check_name("   ").is_err());
        assert!(check_name(&"x".repeat(65)).is_err());
        assert!(check_name(&"é".repeat(64)).is_ok());
        assert!(check_name("bad\nname").is_err());
    }
}
