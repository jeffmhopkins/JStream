//! The control token file (ADR-0006: the token lives in a file only the user can read),
//! for the hosts that let outside clients in: the daemon always, the desktop app with
//! `--mcp`.
//!
//! A host reuses the token in an existing file, so clients configured with it keep
//! working across restarts, and otherwise creates the file with a new random token.
//! The file holds the token and a newline; `jstream-cli` reads it without the
//! surrounding whitespace.
//!
//! Docs: crates/jstream-server/README.md#design

use crate::Token;
use crate::devices::create_private;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Random bytes in a new token: 256 bits.
const TOKEN_BYTES: usize = 32;

/// Where the token came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    /// The file existed and its token is reused.
    Existing,
    /// The file was created with a new token.
    Created,
}

/// The token a file holds, as [`load_or_create`] read or wrote it.
pub struct Loaded {
    /// The token's text, for the host to register with its crash reports' redaction and,
    /// in the desktop app, to hand its own window. Never logged.
    pub text: String,
    /// The token, for [`ServerConfig::new`](crate::ServerConfig::new).
    pub token: Token,
    /// Whether the file was there.
    pub provenance: Provenance,
}

/// Why the token file can't be used. Its message names the file and what to do.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TokenFileError(String);

/// The file beside the token file that keeps the paired devices (ADR-0044): the token
/// file's name with `.devices` added (`token` gives `token.devices`). The server reads
/// it, creates it user-only, and keeps only the devices' token digests in it.
pub fn device_file(token_file: &Path) -> PathBuf {
    let mut name = token_file.as_os_str().to_owned();
    name.push(".devices");
    PathBuf::from(name)
}

/// Reads the token in `path`, or creates the file with a new token if there is none.
///
/// An existing file must hold a valid token (visible ASCII, after trimming whitespace)
/// and must be user-only: on Unix it gives group and others no permission at all; on
/// Windows its owner is the user, SYSTEM or Administrators and its DACL grants no one
/// else real access (#344, `jstream_platform_windows::check_private_file`). A new file
/// is created atomically (it never replaces a file created meanwhile) and user-only:
/// mode 0600 on Unix, a protected DACL for SYSTEM and the user on Windows.
///
/// # Errors
/// When the file can't be created, read or used, or the OS has no randomness.
pub fn load_or_create(path: &Path) -> Result<Loaded, TokenFileError> {
    let shown = path.display();
    // Drawn first, so a failing random source leaves no empty file behind.
    let text = new_token()?;
    match create_private(path) {
        Ok(file) => {
            if let Err(error) = write_token(file, &text) {
                // Leave no partial file behind to be reused on the next start.
                let _ = fs::remove_file(path);
                return Err(TokenFileError(format!(
                    "could not write the token file `{shown}`: {error}"
                )));
            }
            let token = Token::new(text.clone()).map_err(|error| {
                TokenFileError(format!("invariant: a hex token is valid: {error}"))
            })?;
            Ok(Loaded {
                text,
                token,
                provenance: Provenance::Created,
            })
        }
        // Windows reports a directory in the way as `AccessDenied`, not `AlreadyExists`,
        // so anything already at `path` is judged by `read_existing`.
        Err(error)
            if error.kind() == io::ErrorKind::AlreadyExists
                || fs::symlink_metadata(path).is_ok() =>
        {
            read_existing(path)
        }
        Err(error) => Err(TokenFileError(format!(
            "could not create the token file `{shown}`: {error}"
        ))),
    }
}

fn write_token(mut file: File, token: &str) -> io::Result<()> {
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()
}

/// A new token: [`TOKEN_BYTES`] bytes from the OS's random source, as lowercase hex.
fn new_token() -> Result<String, TokenFileError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|error| TokenFileError(format!("the OS random source failed: {error}")))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn read_existing(path: &Path) -> Result<Loaded, TokenFileError> {
    let shown = path.display();
    let unreadable = |error: io::Error| {
        TokenFileError(format!("could not read the token file `{shown}`: {error}"))
    };
    let metadata = fs::metadata(path).map_err(unreadable)?;
    if !metadata.is_file() {
        return Err(TokenFileError(format!(
            "the token file `{shown}` is not a regular file"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(TokenFileError(format!(
                "the token file `{shown}` is accessible to other users (mode {:o}); \
                 make it user-only (chmod 600) or delete it to get a new token",
                mode & 0o777
            )));
        }
    }
    let mut file = File::open(path).map_err(unreadable)?;
    #[cfg(windows)]
    if let Err(why) = jstream_platform_windows::check_private_file(&file) {
        return Err(TokenFileError(format!(
            "the token file `{shown}` may be accessible to other users: {why}; make it \
             user-only or delete it to get a new token"
        )));
    }
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(unreadable)?;
    let text = text.trim();
    if text.is_empty() {
        return Err(TokenFileError(format!(
            "the token file `{shown}` is empty; delete it to get a new token"
        )));
    }
    let token = Token::new(text).map_err(|error| {
        TokenFileError(format!(
            "the token file `{shown}` holds no valid token: {error}"
        ))
    })?;
    Ok(Loaded {
        text: text.to_owned(),
        token,
        provenance: Provenance::Existing,
    })
}

#[cfg(test)]
mod tests {
    use super::{Provenance, device_file, load_or_create, new_token};
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A fresh, empty directory for one test, under the system's temporary directory.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jstream-server-token-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the scratch directory is created");
        dir
    }

    /// Writes `text` to a new user-only file, as a user would prepare one: mode 0600 on
    /// Unix, and on Windows a protected DACL, since the scratch folder's inherited
    /// entries may let other users read it (CI's runner grants `BUILTIN\Users` read), and
    /// such a file is refused before its contents are looked at.
    fn write_private(path: &Path, text: &str) {
        #[cfg(windows)]
        {
            use std::io::Write;
            let mut file = jstream_platform_windows::create_private_file(path).expect("created");
            file.write_all(text.as_bytes()).expect("written");
        }
        #[cfg(not(windows))]
        fs::write(path, text).expect("written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("chmod");
        }
    }

    #[test]
    fn the_device_file_sits_beside_the_token_file() {
        assert_eq!(
            device_file(Path::new("dir/token")),
            PathBuf::from("dir/token.devices")
        );
    }

    #[test]
    fn new_tokens_are_long_random_hex() {
        let a = new_token().expect("OS randomness");
        let b = new_token().expect("OS randomness");
        assert_eq!(a.len(), 64);
        assert!(
            a.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_ne!(a, b);
    }

    #[test]
    fn a_missing_file_is_created_and_then_reused() {
        let dir = scratch("reuse");
        let path = dir.join("token");
        let created = load_or_create(&path).expect("created");
        assert_eq!(created.provenance, Provenance::Created);
        let written = fs::read_to_string(&path).expect("readable");
        assert!(written.ends_with('\n'));
        assert_eq!(written.trim(), created.text);
        assert_eq!(created.text.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("exists").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "user-only");
        }

        let reused = load_or_create(&path).expect("reused");
        assert_eq!(reused.provenance, Provenance::Existing);
        assert_eq!(reused.text, created.text);
        assert_eq!(
            fs::read_to_string(&path).expect("readable"),
            written,
            "unchanged"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unusable_files_are_refused_and_left_alone() {
        let dir = scratch("refused");
        let empty = dir.join("empty");
        write_private(&empty, "  \n");
        let error = load_or_create(&empty).err().expect("empty");
        assert!(error.to_string().contains("is empty"), "{error}");

        let spaced = dir.join("spaced");
        write_private(&spaced, "two words\n");
        let error = load_or_create(&spaced).err().expect("not a token");
        assert!(
            error.to_string().contains("holds no valid token"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&spaced).expect("kept"), "two words\n");

        let error = load_or_create(&dir).err().expect("a directory");
        assert!(error.to_string().contains("not a regular file"), "{error}");

        let missing_dir = dir.join("missing").join("token");
        let error = load_or_create(&missing_dir)
            .err()
            .expect("no parent directory");
        assert!(error.to_string().contains("could not create"), "{error}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// #344: a token file whose DACL lets everyone read it is refused, and one this
    /// host created passes.
    #[cfg(windows)]
    #[test]
    fn a_file_other_users_can_read_is_refused_on_windows() {
        let dir = scratch("dacl");
        let path = dir.join("token");
        let created = load_or_create(&path).expect("created");
        assert_eq!(created.provenance, Provenance::Created);
        let reused = load_or_create(&path).expect("its own DACL passes");
        assert_eq!(reused.provenance, Provenance::Existing);

        // Everyone (S-1-1-0) may read it now.
        let granted = std::process::Command::new("icacls")
            .arg(&path)
            .args(["/grant", "*S-1-1-0:R"])
            .output()
            .expect("icacls runs");
        assert!(granted.status.success(), "{granted:?}");
        let error = load_or_create(&path).err().expect("everyone can read it");
        assert!(
            error.to_string().contains("accessible to other users"),
            "{error}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_other_users_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        let path = dir.join("token");
        fs::write(&path, "s3cret-T0ken\n").expect("written");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        let error = load_or_create(&path)
            .err()
            .expect("group and others can read it");
        assert!(error.to_string().contains("mode 644"), "{error}");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
        let reused = load_or_create(&path).expect("user-only now");
        assert_eq!(reused.provenance, Provenance::Existing);
        let _ = fs::remove_dir_all(&dir);
    }
}
