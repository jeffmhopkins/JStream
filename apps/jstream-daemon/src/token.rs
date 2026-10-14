//! The control token file (ADR-0006: the token lives in a file only the user can read).
//!
//! The daemon reuses the token in an existing file, so clients configured with it keep
//! working across restarts, and otherwise creates the file with a new random token.
//! The file holds the token and a newline; `jstream-cli` reads it without the
//! surrounding whitespace.

use anyhow::{Context, bail};
use jstream_server::Token;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

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

/// Reads the token in `path`, or creates the file with a new token if there is none.
///
/// An existing file must hold a valid token (visible ASCII, after trimming whitespace)
/// and, on Unix, must give group and others no permission at all. A new file is
/// created atomically (it never replaces a file created meanwhile), user-only on Unix
/// (mode 0600); on Windows it gets its directory's ACL. The token is registered with
/// the crash reports' redaction.
pub fn load_or_create(path: &Path) -> anyhow::Result<(Token, Provenance)> {
    // Drawn first, so a failing random source leaves no empty file behind.
    let token = new_token()?;
    match create_new(path) {
        Ok(file) => {
            if let Err(error) = write_token(file, &token) {
                // Leave no partial file behind to be reused on the next start.
                let _ = fs::remove_file(path);
                return Err(error).with_context(|| {
                    format!("could not write the token file `{}`", path.display())
                });
            }
            crate::crash::register_secret(&token);
            let token = Token::new(token).context("invariant: a hex token is valid")?;
            Ok((token, Provenance::Created))
        }
        // Windows reports a directory in the way as `AccessDenied`, not `AlreadyExists`,
        // so anything already at `path` is judged by `read_existing`.
        Err(error)
            if error.kind() == io::ErrorKind::AlreadyExists
                || fs::symlink_metadata(path).is_ok() =>
        {
            Ok((read_existing(path)?, Provenance::Existing))
        }
        Err(error) => Err(error)
            .with_context(|| format!("could not create the token file `{}`", path.display())),
    }
}

/// Opens a new file at `path`, failing with `AlreadyExists` if anything is there.
fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

fn write_token(mut file: File, token: &str) -> io::Result<()> {
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()
}

/// A new token: [`TOKEN_BYTES`] bytes from the OS's random source, as lowercase hex.
fn new_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!("the OS random source failed: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn read_existing(path: &Path) -> anyhow::Result<Token> {
    let shown = path.display();
    let metadata =
        fs::metadata(path).with_context(|| format!("could not read the token file `{shown}`"))?;
    if !metadata.is_file() {
        bail!("the token file `{shown}` is not a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            bail!(
                "the token file `{shown}` is accessible to other users (mode {:o}); \
                 make it user-only (chmod 600) or delete it to get a new token",
                mode & 0o777
            );
        }
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("could not read the token file `{shown}`"))?;
    let token = text.trim();
    if token.is_empty() {
        bail!("the token file `{shown}` is empty; delete it to get a new token");
    }
    let valid = Token::new(token)
        .with_context(|| format!("the token file `{shown}` holds no valid token"))?;
    crate::crash::register_secret(token);
    Ok(valid)
}

#[cfg(test)]
mod tests {
    use super::{Provenance, load_or_create, new_token};
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A fresh, empty directory for one test, under the system's temporary directory.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jstream-daemon-token-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the scratch directory is created");
        dir
    }

    /// Writes `text` to a user-only file, as a user would prepare one.
    fn write_private(path: &Path, text: &str) {
        fs::write(path, text).expect("written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("chmod");
        }
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
        let (_, provenance) = load_or_create(&path).expect("created");
        assert_eq!(provenance, Provenance::Created);
        let written = fs::read_to_string(&path).expect("readable");
        assert!(written.ends_with('\n'));
        assert_eq!(written.trim().len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("exists").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "user-only");
        }

        let (_, provenance) = load_or_create(&path).expect("reused");
        assert_eq!(provenance, Provenance::Existing);
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
        let error = load_or_create(&empty).expect_err("empty");
        assert!(error.to_string().contains("is empty"), "{error}");

        let spaced = dir.join("spaced");
        write_private(&spaced, "two words\n");
        let error = load_or_create(&spaced).expect_err("not a token");
        assert!(
            error.to_string().contains("holds no valid token"),
            "{error:#}"
        );
        assert_eq!(fs::read_to_string(&spaced).expect("kept"), "two words\n");

        let error = load_or_create(&dir).expect_err("a directory");
        assert!(error.to_string().contains("not a regular file"), "{error}");

        let missing_dir = dir.join("missing").join("token");
        let error = load_or_create(&missing_dir).expect_err("no parent directory");
        assert!(error.to_string().contains("could not create"), "{error}");
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
        let error = load_or_create(&path).expect_err("group and others can read it");
        assert!(error.to_string().contains("mode 644"), "{error}");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
        let (_, provenance) = load_or_create(&path).expect("user-only now");
        assert_eq!(provenance, Provenance::Existing);
        let _ = fs::remove_dir_all(&dir);
    }
}
