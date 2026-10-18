//! The store itself: the Secret Service client, the fallback file and the moves between
//! them (ADR-0071 §1-§3). Linux only.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jstream_platform::{OAuthTokenStore, PlatformError, Secret, StreamKeyStore};

use super::service::{self, BUS_NAME, NONE, PROMPT};
use super::{
    Contents, Entry, FILE_NAME, Kind, Name, check_secret, fallback_warning, holds_secrets, parse,
    serialize,
};
use crate::dbus::bus::{BUS, Bus, Received};
use crate::dbus::{Kind as MessageKind, Message, Value, add_match, match_rule};

/// How long a Secret Service call may take, the D-Bus default. Starting the service on
/// the first call is part of it.
const CALL: Duration = Duration::from_secs(25);

/// How long the user has to answer the keyring's unlock prompt.
const PROMPT_WAIT: Duration = Duration::from_secs(300);

/// The secrets of one place: the desktop's keyring through the Secret Service, or the
/// fallback file where no Secret Service is reachable (ADR-0071).
///
/// Every call opens the session bus and a Secret Service session, so a keyring that
/// starts later is used from the next call on, and the file's secrets move into it then.
/// A call may block while the keyring's unlock prompt is open, up to five minutes. Calls
/// in one process take turns; the file is replaced by renaming, so a reader in another
/// process never sees half of it.
///
/// Secrets read come back as [`Secret`]s, zeroed on drop. The D-Bus client zeroes the
/// messages it encodes and decodes; copies made by the kernel, the keyring and buffers
/// that grew are not reached, as with the other platforms' stores.
pub struct LinuxSecrets {
    file: PathBuf,
    lock: Mutex<()>,
    connect: Box<Connect>,
}

/// Opens the keyring for one call: `None` when no Secret Service is reachable.
type Connect = dyn Fn() -> Result<Option<Box<dyn Vault>>, PlatformError> + Send + Sync;

impl std::fmt::Debug for LinuxSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxSecrets")
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

impl LinuxSecrets {
    /// The store whose fallback file is `secrets` in `config_dir`, normally
    /// [`config_dir`](Self::config_dir).
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self::with_connect(config_dir.into(), || {
            Ok(Keyring::connect()?.map(|k| Box::new(k) as Box<dyn Vault>))
        })
    }

    fn with_connect(
        config_dir: PathBuf,
        connect: impl Fn() -> Result<Option<Box<dyn Vault>>, PlatformError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            file: config_dir.join(FILE_NAME),
            lock: Mutex::new(()),
            connect: Box::new(connect),
        }
    }

    /// JStream's config folder: `$XDG_CONFIG_HOME/jstream`, else `~/.config/jstream`.
    /// Only absolute variables count. `None` when neither is set.
    pub fn config_dir() -> Option<PathBuf> {
        let absolute = |name: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        absolute("XDG_CONFIG_HOME")
            .map(|d| d.join("jstream"))
            .or_else(|| absolute("HOME").map(|h| h.join(".config/jstream")))
    }

    /// The fallback file.
    pub fn file_path(&self) -> &Path {
        &self.file
    }

    /// The warning to show and log while the fallback holds a secret (ADR-0071 §4), or
    /// `None` when it holds none. Where a Secret Service is reachable, the file's
    /// secrets move into the keyring first, so the answer is `None` once they have. A
    /// host calls it at startup and logs the warning once; it names the file, never a
    /// secret.
    ///
    /// # Errors
    /// The keyring failed while the secrets moved, or the file can't be read.
    pub fn fallback_warning(&self) -> Result<Option<String>, PlatformError> {
        let _turn = self.turn();
        if let Some(mut keyring) = (self.connect)()? {
            migrate(&self.file, keyring.as_mut())?;
            return Ok(None);
        }
        Ok(holds_secrets(&load(&self.file)?).then(|| fallback_warning(&self.file)))
    }

    fn turn(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Runs `op` on the keyring, after moving the file's secrets into it, or on the file
    /// when no Secret Service is reachable.
    fn with<T>(
        &self,
        op: impl FnOnce(&mut dyn Vault) -> Result<T, PlatformError>,
    ) -> Result<T, PlatformError> {
        let _turn = self.turn();
        match (self.connect)()? {
            Some(mut keyring) => {
                migrate(&self.file, keyring.as_mut())?;
                op(keyring.as_mut())
            }
            None => op(&mut FileVault(&self.file)),
        }
    }

    fn set(&self, name: &Name, secret: &Secret) -> Result<(), PlatformError> {
        check_secret(secret)?;
        self.with(|v| v.set(name, secret))
    }
}

/// Stream keys, by output ID. An empty or control-character output ID, or an empty key,
/// is `Unsupported`.
impl StreamKeyStore for LinuxSecrets {
    fn set(&self, output_id: &str, key: &Secret) -> Result<(), PlatformError> {
        Self::set(self, &Name::new(Kind::StreamKey, output_id)?, key)
    }

    fn get(&self, output_id: &str) -> Result<Option<Secret>, PlatformError> {
        let name = Name::new(Kind::StreamKey, output_id)?;
        self.with(|v| v.get(&name))
    }

    fn clear(&self, output_id: &str) -> Result<(), PlatformError> {
        let name = Name::new(Kind::StreamKey, output_id)?;
        self.with(|v| v.clear(&name))
    }

    fn is_set(&self, output_id: &str) -> Result<bool, PlatformError> {
        let name = Name::new(Kind::StreamKey, output_id)?;
        self.with(|v| v.is_set(&name))
    }
}

/// OAuth refresh tokens, by account ID, apart from stream keys. Checked as stream keys
/// are.
impl OAuthTokenStore for LinuxSecrets {
    fn set(&self, account: &str, token: &Secret) -> Result<(), PlatformError> {
        Self::set(self, &Name::new(Kind::OAuthToken, account)?, token)
    }

    fn get(&self, account: &str) -> Result<Option<Secret>, PlatformError> {
        let name = Name::new(Kind::OAuthToken, account)?;
        self.with(|v| v.get(&name))
    }

    fn clear(&self, account: &str) -> Result<(), PlatformError> {
        let name = Name::new(Kind::OAuthToken, account)?;
        self.with(|v| v.clear(&name))
    }

    fn is_set(&self, account: &str) -> Result<bool, PlatformError> {
        let name = Name::new(Kind::OAuthToken, account)?;
        self.with(|v| v.is_set(&name))
    }
}

/// Where secrets are kept for one call: the keyring or the file.
trait Vault {
    fn set(&mut self, name: &Name, secret: &Secret) -> Result<(), PlatformError>;
    fn get(&mut self, name: &Name) -> Result<Option<Secret>, PlatformError>;
    fn clear(&mut self, name: &Name) -> Result<(), PlatformError>;
    fn is_set(&mut self, name: &Name) -> Result<bool, PlatformError>;
}

/// Moves the file's entries into `keyring`: a secret is stored there, a cleared entry
/// deletes the keyring's copy. Each entry leaves the file once it has moved, and the file
/// is deleted when it is empty. On an error, the entries not yet moved stay in the file.
fn migrate(file: &Path, keyring: &mut dyn Vault) -> Result<(), PlatformError> {
    let mut contents = load(file)?;
    if contents.is_empty() {
        return match fs::symlink_metadata(file) {
            Ok(_) => save(file, &contents),
            Err(_) => Ok(()),
        };
    }
    let names: Vec<Name> = contents.keys().cloned().collect();
    for name in names {
        let moved = match contents.get(&name) {
            Some(Entry::Set(secret)) => keyring.set(&name, secret),
            Some(Entry::Cleared) => keyring.clear(&name),
            None => Ok(()),
        };
        if let Err(e) = moved {
            save(file, &contents)?;
            return Err(e);
        }
        contents.remove(&name);
    }
    save(file, &contents)
}

/// The fallback file as a [`Vault`]: each call reads it and, for a change, replaces it.
struct FileVault<'a>(&'a Path);

impl Vault for FileVault<'_> {
    fn set(&mut self, name: &Name, secret: &Secret) -> Result<(), PlatformError> {
        let mut contents = load(self.0)?;
        contents.insert(
            name.clone(),
            Entry::Set(Secret::new(secret.expose().to_owned())),
        );
        save(self.0, &contents)
    }

    fn get(&mut self, name: &Name) -> Result<Option<Secret>, PlatformError> {
        Ok(match load(self.0)?.remove(name) {
            Some(Entry::Set(secret)) => Some(secret),
            _ => None,
        })
    }

    /// Records the clear, so the keyring's copy, if it has one, is deleted when a
    /// keyring is reachable again.
    fn clear(&mut self, name: &Name) -> Result<(), PlatformError> {
        let mut contents = load(self.0)?;
        contents.insert(name.clone(), Entry::Cleared);
        save(self.0, &contents)
    }

    fn is_set(&mut self, name: &Name) -> Result<bool, PlatformError> {
        Ok(matches!(load(self.0)?.get(name), Some(Entry::Set(_))))
    }
}

fn file_error(call: &'static str, path: &Path, e: &std::io::Error) -> PlatformError {
    PlatformError::Os {
        call,
        code: e.raw_os_error().and_then(|c| u32::try_from(c).ok()),
        message: format!("{}: {e}", path.display()),
    }
}

/// The file's entries; none when there is no file. The file must be a regular file (not
/// a symlink) that this user owns and nobody else can read or write: anything else is
/// refused, never repaired (ADR-0071 §2).
fn load(path: &Path) -> Result<Contents, PlatformError> {
    const CALL: &str = "reading the secrets file";
    let mut f = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Contents::new()),
        Err(e) => return Err(file_error(CALL, path, &e)),
    };
    let meta = f.metadata().map_err(|e| file_error(CALL, path, &e))?;
    // SAFETY: `uid_t geteuid(void)` has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if !meta.is_file() || meta.uid() != me || meta.mode() & 0o077 != 0 {
        return Err(PlatformError::Os {
            call: CALL,
            code: None,
            message: format!(
                "{} is not a regular file that only this user owns and can read (mode {:o}); \
                 JStream won't use it. Make it user-only (chmod 600) if it is yours, or \
                 delete it",
                path.display(),
                meta.mode() & 0o777
            ),
        });
    }
    let len = usize::try_from(meta.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(len);
    let read = f.read_to_end(&mut bytes);
    let text = Secret::from_utf8(bytes)?;
    read.map_err(|e| file_error(CALL, path, &e))?;
    parse(text.expose())
}

/// Replaces the file with `contents`, or deletes it when they are empty. The folder is
/// created `0700` when missing (an existing one is left as it is); the text goes to a new
/// `0600` file beside it, which is then renamed over the file.
fn save(path: &Path, contents: &Contents) -> Result<(), PlatformError> {
    const CALL: &str = "writing the secrets file";
    if contents.is_empty() {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(file_error(CALL, path, &e)),
        };
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| file_error(CALL, dir, &e))?;
    // A name of this process's own, so two processes writing at once don't remove
    // each other's temporary file; the last rename wins.
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    match fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(file_error(CALL, &tmp, &e)),
    }
    let text = serialize(contents);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(text.expose().as_bytes())?;
            f.sync_all()
        })
        .and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(file_error(CALL, path, &e));
    }
    Ok(())
}

/// A connection to the Secret Service with an open `plain` session.
struct Keyring {
    bus: Bus,
    /// The service's unique name: replies and signals from anyone else are dropped.
    owner: String,
    session: String,
}

impl Keyring {
    /// Connects, starting the service if the bus can. `None` when no Secret Service is
    /// reachable: no session bus, or nothing provides `org.freedesktop.secrets`.
    fn connect() -> Result<Option<Self>, PlatformError> {
        const WHAT: &str = "org.freedesktop.Secret.Service.OpenSession";
        let mut bus = match Bus::session() {
            Ok(bus) => bus,
            Err(e) if no_session_bus(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        let reply = bus.call_reply(&service::open_session(), None, WHAT, CALL)?;
        if reply.msg.kind == MessageKind::Error {
            let name = reply.msg.error_name.clone().unwrap_or_default();
            if service::unreachable(&name) {
                return Ok(None);
            }
            return Err(error_reply(&reply, WHAT));
        }
        let session = service::object_path(reply.msg.body.get(1), WHAT)?;
        let owner = bus
            .call(
                Message::call(
                    BUS,
                    "/org/freedesktop/DBus",
                    BUS,
                    "GetNameOwner",
                    vec![Value::Str(BUS_NAME.into())],
                ),
                Some(BUS),
                "org.freedesktop.DBus.GetNameOwner",
                CALL,
            )?
            .msg
            .body
            .first()
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| PlatformError::Os {
                call: "org.freedesktop.DBus.GetNameOwner",
                code: None,
                message: "the reply has no name".into(),
            })?;
        Ok(Some(Self {
            bus,
            owner,
            session,
        }))
    }

    fn call(&mut self, msg: &Message, what: &'static str) -> Result<Received, PlatformError> {
        let reply = self.bus.call_reply(msg, Some(&self.owner), what, CALL)?;
        if reply.msg.kind == MessageKind::Error {
            return Err(error_reply(&reply, what));
        }
        Ok(reply)
    }

    /// Runs `prompt` unless it is `/`, and returns its result. A dismissed prompt is an
    /// error.
    fn prompt(&mut self, prompt: &str, what: &'static str) -> Result<Option<Value>, PlatformError> {
        if prompt == NONE {
            return Ok(None);
        }
        let rule = format!(
            "{},path='{prompt}'",
            match_rule(&self.owner, PROMPT, "Completed")
        );
        self.bus.call(
            add_match(&rule),
            Some(BUS),
            "org.freedesktop.DBus.AddMatch",
            CALL,
        )?;
        self.call(
            &service::prompt(prompt),
            "org.freedesktop.Secret.Prompt.Prompt",
        )?;
        let owner = self.owner.clone();
        let done = self
            .bus
            .wait_signal(Instant::now() + PROMPT_WAIT, |m| {
                m.is_signal(PROMPT, "Completed")
                    && m.path.as_deref() == Some(prompt)
                    && m.sender.as_deref() == Some(owner.as_str())
            })?
            .ok_or_else(|| PlatformError::Os {
                call: what,
                code: None,
                message: "the keyring's prompt got no answer within five minutes".into(),
            })?;
        match service::completed(&done.body)? {
            Some(result) => Ok(Some(result.clone())),
            None => Err(PlatformError::Os {
                call: what,
                code: None,
                message: "the keyring's prompt was dismissed, so nothing was stored or read".into(),
            }),
        }
    }

    /// Unlocks `objects`, prompting if the keyring asks to.
    fn unlock(&mut self, objects: &[String]) -> Result<(), PlatformError> {
        const WHAT: &str = "org.freedesktop.Secret.Service.Unlock";
        if objects.is_empty() {
            return Ok(());
        }
        let reply = self.call(&service::unlock(objects), WHAT)?;
        let body = &reply.msg.body;
        let mut unlocked = match body.first() {
            Some(v) => service::object_paths(v, WHAT)?,
            None => Vec::new(),
        };
        let prompt = service::object_path(body.get(1), WHAT)?;
        if let Some(result) = self.prompt(&prompt, WHAT)? {
            unlocked.extend(service::object_paths(&result, WHAT)?);
        }
        if objects.iter().all(|o| unlocked.contains(o)) {
            Ok(())
        } else {
            Err(PlatformError::Os {
                call: WHAT,
                code: None,
                message: "the keyring stayed locked".into(),
            })
        }
    }

    /// The items named `name`: unlocked ones, then locked ones.
    fn search(&mut self, name: &Name) -> Result<(Vec<String>, Vec<String>), PlatformError> {
        const WHAT: &str = "org.freedesktop.Secret.Service.SearchItems";
        let reply = self.call(&service::search_items(name), WHAT)?;
        let [unlocked, locked] = reply.msg.body.as_slice() else {
            return Err(PlatformError::Os {
                call: WHAT,
                code: None,
                message: "the Secret Service's reply has an unexpected shape".into(),
            });
        };
        Ok((
            service::object_paths(unlocked, WHAT)?,
            service::object_paths(locked, WHAT)?,
        ))
    }

    /// The default collection, created (with the keyring's prompt) when the keyring has
    /// none, and unlocked.
    fn default_collection(&mut self) -> Result<String, PlatformError> {
        const WHAT: &str = "org.freedesktop.Secret.Service.CreateCollection";
        let alias = self.call(
            &service::read_default_alias(),
            "org.freedesktop.Secret.Service.ReadAlias",
        )?;
        let mut collection = service::object_path(
            alias.msg.body.first(),
            "org.freedesktop.Secret.Service.ReadAlias",
        )?;
        if collection == NONE {
            let reply = self.call(&service::create_default_collection(), WHAT)?;
            collection = service::object_path(reply.msg.body.first(), WHAT)?;
            let prompt = service::object_path(reply.msg.body.get(1), WHAT)?;
            if let Some(created) = self.prompt(&prompt, WHAT)? {
                collection = service::object_path(Some(&created), WHAT)?;
            }
            if collection == NONE {
                return Err(PlatformError::Os {
                    call: WHAT,
                    code: None,
                    message: "the keyring created no default collection".into(),
                });
            }
        }
        self.unlock(std::slice::from_ref(&collection))?;
        Ok(collection)
    }
}

impl Vault for Keyring {
    fn set(&mut self, name: &Name, secret: &Secret) -> Result<(), PlatformError> {
        const WHAT: &str = "org.freedesktop.Secret.Collection.CreateItem";
        let collection = self.default_collection()?;
        let mut msg = service::create_item(&collection, name, &self.session, secret);
        let reply = self.call(&msg, WHAT);
        service::wipe(&mut msg.body);
        let reply = reply?;
        let prompt = service::object_path(reply.msg.body.get(1), WHAT)?;
        self.prompt(&prompt, WHAT).map(drop)
    }

    fn get(&mut self, name: &Name) -> Result<Option<Secret>, PlatformError> {
        const WHAT: &str = "org.freedesktop.Secret.Item.GetSecret";
        let (unlocked, locked) = self.search(name)?;
        let item = match (unlocked.first(), locked.first()) {
            (Some(item), _) => item.clone(),
            (None, Some(item)) => {
                let item = item.clone();
                self.unlock(std::slice::from_ref(&item))?;
                item
            }
            (None, None) => return Ok(None),
        };
        let mut reply = self.call(&service::get_secret(&item, &self.session), WHAT)?;
        let secret = service::secret_of(&reply.msg.body);
        service::wipe(&mut reply.msg.body);
        secret.map(Some)
    }

    fn clear(&mut self, name: &Name) -> Result<(), PlatformError> {
        const WHAT: &str = "org.freedesktop.Secret.Item.Delete";
        let (unlocked, locked) = self.search(name)?;
        self.unlock(&locked)?;
        for item in unlocked.iter().chain(&locked) {
            let reply = self.call(&service::delete(item), WHAT)?;
            let prompt = service::object_path(reply.msg.body.first(), WHAT)?;
            self.prompt(&prompt, WHAT)?;
        }
        Ok(())
    }

    fn is_set(&mut self, name: &Name) -> Result<bool, PlatformError> {
        let (unlocked, locked) = self.search(name)?;
        Ok(!unlocked.is_empty() || !locked.is_empty())
    }
}

/// Whether opening the session bus failed because there is none (ADR-0071 §2): no
/// address to connect to, or a socket that isn't there or refuses connections. A bus
/// that is there but fails (authentication, `Hello`, a timeout) is an error, never a
/// reason to use the file.
fn no_session_bus(error: &PlatformError) -> bool {
    match error {
        PlatformError::MissingComponent { .. } => true,
        PlatformError::Os {
            call: "connecting to the D-Bus session bus",
            code: Some(code),
            ..
        } => [libc::ENOENT, libc::ECONNREFUSED]
            .into_iter()
            .any(|c| u32::try_from(c).ok() == Some(*code)),
        _ => false,
    }
}

/// The error for a Secret Service error reply: its name and message, which name no
/// secret.
fn error_reply(reply: &Received, what: &'static str) -> PlatformError {
    let name = reply.msg.error_name.clone().unwrap_or_default();
    let text = reply
        .msg
        .body
        .first()
        .and_then(Value::as_str)
        .unwrap_or_default();
    PlatformError::Os {
        call: what,
        code: None,
        message: format!("{name}: {text}"),
    }
}

#[cfg(test)]
mod tests;
