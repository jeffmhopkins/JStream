use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use jstream_platform::{OAuthTokenStore, PlatformError, Secret, StreamKeyStore};

use super::{LinuxSecrets, Vault};
use crate::secrets::Name;

/// A fresh, empty folder for one test, deleted when the test ends.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch(tag: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("jstream-secrets-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("a scratch folder");
    Scratch(dir)
}

fn mode(path: &std::path::Path) -> u32 {
    fs::metadata(path).expect("exists").permissions().mode() & 0o777
}

/// A keyring in memory, shared with the test, that can be told to fail.
#[derive(Clone, Default)]
struct Memory {
    items: Arc<Mutex<BTreeMap<Name, String>>>,
    failing: Arc<Mutex<bool>>,
}

impl Memory {
    fn check(&self) -> Result<(), PlatformError> {
        if *self.failing.lock().expect("lock") {
            return Err(PlatformError::Os {
                call: "test keyring",
                code: None,
                message: "refused".into(),
            });
        }
        Ok(())
    }

    fn ids(&self) -> Vec<String> {
        self.items
            .lock()
            .expect("lock")
            .iter()
            .map(|(n, s)| format!("{}={s}", n.label()))
            .collect()
    }
}

impl Vault for Memory {
    fn set(&mut self, name: &Name, secret: &Secret) -> Result<(), PlatformError> {
        self.check()?;
        self.items
            .lock()
            .expect("lock")
            .insert(name.clone(), secret.expose().to_owned());
        Ok(())
    }
    fn get(&mut self, name: &Name) -> Result<Option<Secret>, PlatformError> {
        self.check()?;
        Ok(self.items.lock().expect("lock").get(name).map(Secret::new))
    }
    fn clear(&mut self, name: &Name) -> Result<(), PlatformError> {
        self.check()?;
        self.items.lock().expect("lock").remove(name);
        Ok(())
    }
    fn is_set(&mut self, name: &Name) -> Result<bool, PlatformError> {
        self.check()?;
        Ok(self.items.lock().expect("lock").contains_key(name))
    }
}

/// A store over `dir` whose keyring is `memory` while `reachable` holds, and
/// unreachable otherwise.
fn store(dir: &std::path::Path, memory: &Memory, reachable: &Arc<Mutex<bool>>) -> LinuxSecrets {
    let (memory, reachable) = (memory.clone(), Arc::clone(reachable));
    LinuxSecrets::with_connect(dir.to_owned(), move || {
        Ok((*reachable.lock().expect("lock")).then(|| Box::new(memory.clone()) as Box<dyn Vault>))
    })
}

fn get(keys: &dyn StreamKeyStore, id: &str) -> Option<String> {
    keys.get(id).expect("get").map(|s| s.expose().to_owned())
}

#[test]
fn without_a_keyring_secrets_go_to_a_private_file() {
    let scratch = scratch("file");
    let dir = scratch.0.join("jstream");
    let memory = Memory::default();
    let store = store(&dir, &memory, &Arc::new(Mutex::new(false)));
    assert_eq!(store.file_path(), dir.join("secrets"));
    assert_eq!(store.fallback_warning().expect("no file"), None);

    StreamKeyStore::set(&store, "out_1", &Secret::new("live_abc")).expect("set");
    OAuthTokenStore::set(&store, "out_1", &Secret::new("1//refresh")).expect("set token");
    assert_eq!(mode(&dir), 0o700, "the folder is created user-only");
    assert_eq!(mode(store.file_path()), 0o600, "the file is user-only");
    assert_eq!(get(&store, "out_1").as_deref(), Some("live_abc"));
    assert_eq!(
        OAuthTokenStore::get(&store, "out_1")
            .expect("get")
            .map(|s| s.expose().to_owned())
            .as_deref(),
        Some("1//refresh"),
        "a token and a key with one ID are apart"
    );
    assert!(StreamKeyStore::is_set(&store, "out_1").expect("is_set"));
    assert!(!StreamKeyStore::is_set(&store, "out_2").expect("is_set"));
    let warning = store.fallback_warning().expect("reads").expect("a warning");
    assert!(
        warning.contains(&store.file_path().display().to_string()),
        "{warning}"
    );
    assert!(!warning.contains("live_abc"), "{warning}");

    StreamKeyStore::set(&store, "out_1", &Secret::new("live_new")).expect("replace");
    assert_eq!(get(&store, "out_1").as_deref(), Some("live_new"));
    StreamKeyStore::clear(&store, "out_1").expect("clear");
    OAuthTokenStore::clear(&store, "out_1").expect("clear");
    assert_eq!(get(&store, "out_1"), None);
    assert_eq!(
        store.fallback_warning().expect("reads"),
        None,
        "cleared entries hold no secret"
    );
    assert!(memory.ids().is_empty(), "the keyring was never reached");
    assert!(StreamKeyStore::set(&store, "out_1", &Secret::new("")).is_err());
    assert!(StreamKeyStore::set(&store, "", &Secret::new("k")).is_err());
}

#[test]
fn a_file_others_can_read_is_refused_and_not_repaired() {
    let scratch = scratch("perms");
    let dir = scratch.0.clone();
    let store = store(&dir, &Memory::default(), &Arc::new(Mutex::new(false)));
    StreamKeyStore::set(&store, "out", &Secret::new("live_abc")).expect("set");
    fs::set_permissions(store.file_path(), fs::Permissions::from_mode(0o644)).expect("chmod");
    let err = StreamKeyStore::get(&store, "out")
        .expect_err("refused")
        .to_string();
    assert!(err.contains("chmod 600") && err.contains("644"), "{err}");
    assert!(!err.contains("live_abc"), "{err}");
    assert!(StreamKeyStore::set(&store, "out", &Secret::new("x")).is_err());
    assert!(store.fallback_warning().is_err());
    assert_eq!(mode(store.file_path()), 0o644, "never repaired silently");
}

#[test]
fn a_symlinked_file_is_refused() {
    let scratch = scratch("symlink");
    let dir = scratch.0.clone();
    let target = dir.join("elsewhere");
    fs::write(&target, "").expect("write");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("chmod");
    std::os::unix::fs::symlink(&target, dir.join("secrets")).expect("symlink");
    let store = store(&dir, &Memory::default(), &Arc::new(Mutex::new(false)));
    assert!(StreamKeyStore::get(&store, "out").is_err());
}

#[test]
fn a_reachable_keyring_takes_the_files_secrets_and_clears() {
    let scratch = scratch("migrate");
    let dir = scratch.0.clone();
    let memory = Memory::default();
    let reachable = Arc::new(Mutex::new(true));
    let store = store(&dir, &memory, &reachable);

    // Stored while the keyring ran.
    StreamKeyStore::set(&store, "old", &Secret::new("in_keyring")).expect("set");
    StreamKeyStore::set(&store, "kept", &Secret::new("stays")).expect("set");
    assert!(!store.file_path().exists(), "the keyring was used");

    // Changed while it didn't.
    *reachable.lock().expect("lock") = false;
    StreamKeyStore::set(&store, "new", &Secret::new("in_file")).expect("set");
    StreamKeyStore::clear(&store, "old").expect("clear");
    assert_eq!(
        get(&store, "kept"),
        None,
        "only the file is read without a keyring"
    );
    assert!(store.fallback_warning().expect("reads").is_some());

    // Back: the file moves into the keyring, and the cleared key doesn't come back.
    *reachable.lock().expect("lock") = true;
    assert_eq!(store.fallback_warning().expect("moves"), None);
    assert!(!store.file_path().exists(), "the empty file is deleted");
    assert_eq!(
        memory.ids(),
        [
            "JStream/stream-key/kept=stays".to_owned(),
            "JStream/stream-key/new=in_file".to_owned(),
        ]
    );
    assert_eq!(get(&store, "new").as_deref(), Some("in_file"));
}

#[test]
fn a_failing_keyring_never_falls_back_and_keeps_the_file() {
    let scratch = scratch("failing");
    let dir = scratch.0.clone();
    let memory = Memory::default();
    let reachable = Arc::new(Mutex::new(false));
    let store = store(&dir, &memory, &reachable);
    StreamKeyStore::set(&store, "a", &Secret::new("one")).expect("set");
    StreamKeyStore::set(&store, "b", &Secret::new("two")).expect("set");

    *reachable.lock().expect("lock") = true;
    *memory.failing.lock().expect("lock") = true;
    let err =
        StreamKeyStore::set(&store, "c", &Secret::new("three")).expect_err("the keyring refused");
    assert!(!err.to_string().contains("three"), "{err}");
    assert!(store.fallback_warning().is_err());

    // Nothing moved and nothing went to the file instead.
    *reachable.lock().expect("lock") = false;
    assert_eq!(get(&store, "a").as_deref(), Some("one"));
    assert_eq!(get(&store, "b").as_deref(), Some("two"));
    assert_eq!(get(&store, "c"), None, "a refused call never uses the file");

    *reachable.lock().expect("lock") = true;
    *memory.failing.lock().expect("lock") = false;
    StreamKeyStore::set(&store, "c", &Secret::new("three")).expect("set");
    assert_eq!(memory.ids().len(), 3);
    assert!(!store.file_path().exists());
}

/// Against a real Secret Service (R-TEST-004): behind `hw-tests` and `#[ignore]`d,
/// because CI has none. A desktop session's keyring will do, or `dbus-run-session --
/// sh -c 'echo | gnome-keyring-daemon --unlock --components=secrets && cargo test -p
/// jstream-platform-linux --features hw-tests -- --ignored secret_service'`.
#[cfg(feature = "hw-tests")]
#[test]
#[ignore = "needs a Secret Service on the session bus"]
fn round_trips_through_the_secret_service() {
    let scratch = scratch("real");
    let dir = scratch.0.clone();
    let store = LinuxSecrets::new(&dir);
    let id = format!("test-{}", std::process::id());
    let keys: &dyn StreamKeyStore = &store;
    let tokens: &dyn OAuthTokenStore = &store;
    struct Cleanup<'a>(&'a dyn StreamKeyStore, &'a dyn OAuthTokenStore, &'a str);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.clear(self.2);
            let _ = self.1.clear(self.2);
        }
    }
    let _guard = Cleanup(keys, tokens, &id);

    assert!(!keys.is_set(&id).expect("a reachable Secret Service"));
    keys.set(&id, &Secret::new("live_abc é")).expect("set");
    tokens
        .set(&id, &Secret::new("1//refresh"))
        .expect("set token");
    assert!(
        !store.file_path().exists(),
        "the keyring was used, not the file"
    );
    assert_eq!(get(keys, &id).as_deref(), Some("live_abc é"));
    keys.set(&id, &Secret::new("replaced")).expect("replace");
    assert_eq!(get(keys, &id).as_deref(), Some("replaced"));
    tokens.clear(&id).expect("clear");
    assert!(!tokens.is_set(&id).expect("is_set"));
    assert!(keys.is_set(&id).expect("the key stays"));
    keys.clear(&id).expect("clear");
    keys.clear(&id).expect("clearing nothing succeeds");
    assert!(!keys.is_set(&id).expect("is_set"));
    assert_eq!(store.fallback_warning().expect("reachable"), None);
}
