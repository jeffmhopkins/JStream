//! The control token file (ADR-0006: the token lives in a file only the user can read).
//!
//! The daemon reuses the token in an existing file, so clients configured with it keep
//! working across restarts, and otherwise creates the file with a new random token.
//! The file holds the token and a newline; `jstream-cli` reads it without the
//! surrounding whitespace.

use anyhow::{Context, bail};
use jstream_crash::Secrets;
use jstream_server::Token;
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

/// The file beside the token file that keeps the paired devices (ADR-0044): the token
/// file's name with `.devices` added (`token` gives `token.devices`). `jstream-server`
/// reads it, creates it user-only, and keeps only the devices' token digests in it.
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
/// mode 0600 on Unix, a protected DACL for SYSTEM and the user on Windows. The token is
/// registered with the crash reports' redaction (`secrets`).
pub fn load_or_create(path: &Path, secrets: &Secrets) -> anyhow::Result<(Token, Provenance)> {
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
            secrets.register(&token);
            let token = Token::new(token).context("invariant: a hex token is valid")?;
            Ok((token, Provenance::Created))
        }
        // Windows reports a directory in the way as `AccessDenied`, not `AlreadyExists`,
        // so anything already at `path` is judged by `read_existing`.
        Err(error)
            if error.kind() == io::ErrorKind::AlreadyExists
                || fs::symlink_metadata(path).is_ok() =>
        {
            Ok((read_existing(path, secrets)?, Provenance::Existing))
        }
        Err(error) => Err(error)
            .with_context(|| format!("could not create the token file `{}`", path.display())),
    }
}

/// Opens a new user-only file at `path`, failing with `AlreadyExists` if anything is
/// there.
fn create_new(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    return jstream_platform_windows::create_private_file(path);
    #[cfg(not(windows))]
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(path)
    }
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

fn read_existing(path: &Path, secrets: &Secrets) -> anyhow::Result<Token> {
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
    let mut file =
        File::open(path).with_context(|| format!("could not read the token file `{shown}`"))?;
    #[cfg(windows)]
    if let Err(why) = jstream_platform_windows::check_private_file(&file) {
        bail!(
            "the token file `{shown}` may be accessible to other users: {why}; make it \
             user-only or delete it to get a new token"
        );
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .with_context(|| format!("could not read the token file `{shown}`"))?;
    let token = text.trim();
    if token.is_empty() {
        bail!("the token file `{shown}` is empty; delete it to get a new token");
    }
    let valid = Token::new(token)
        .with_context(|| format!("the token file `{shown}` holds no valid token"))?;
    secrets.register(token);
    Ok(valid)
}

#[cfg(test)]
mod tests {
    use super::{Provenance, device_file, load_or_create, new_token};
    use jstream_crash::Secrets;
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
        let created = Secrets::new();
        let (_, provenance) = load_or_create(&path, &created).expect("created");
        assert_eq!(provenance, Provenance::Created);
        let written = fs::read_to_string(&path).expect("readable");
        assert!(written.ends_with('\n'));
        assert_eq!(written.trim().len(), 64);
        assert!(
            created.is_registered(written.trim()),
            "a new token is redacted from crash reports"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("exists").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "user-only");
        }

        let reused = Secrets::new();
        let (_, provenance) = load_or_create(&path, &reused).expect("reused");
        assert_eq!(provenance, Provenance::Existing);
        assert!(
            reused.is_registered(written.trim()),
            "a reused token is redacted from crash reports"
        );
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
        let error = load_or_create(&empty, &Secrets::new()).expect_err("empty");
        assert!(error.to_string().contains("is empty"), "{error}");

        let spaced = dir.join("spaced");
        write_private(&spaced, "two words\n");
        let error = load_or_create(&spaced, &Secrets::new()).expect_err("not a token");
        assert!(
            error.to_string().contains("holds no valid token"),
            "{error:#}"
        );
        assert_eq!(fs::read_to_string(&spaced).expect("kept"), "two words\n");

        let error = load_or_create(&dir, &Secrets::new()).expect_err("a directory");
        assert!(error.to_string().contains("not a regular file"), "{error}");

        let missing_dir = dir.join("missing").join("token");
        let error = load_or_create(&missing_dir, &Secrets::new()).expect_err("no parent directory");
        assert!(error.to_string().contains("could not create"), "{error}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// #344: a token file whose DACL lets everyone read it is refused, and one this
    /// daemon created passes.
    #[cfg(windows)]
    #[test]
    fn a_file_other_users_can_read_is_refused_on_windows() {
        let dir = scratch("dacl");
        let path = dir.join("token");
        let (_, provenance) = load_or_create(&path, &Secrets::new()).expect("created");
        assert_eq!(provenance, Provenance::Created);
        let (_, provenance) = load_or_create(&path, &Secrets::new()).expect("its own DACL passes");
        assert_eq!(provenance, Provenance::Existing);

        // Everyone (S-1-1-0) may read it now.
        let granted = std::process::Command::new("icacls")
            .arg(&path)
            .args(["/grant", "*S-1-1-0:R"])
            .output()
            .expect("icacls runs");
        assert!(granted.status.success(), "{granted:?}");
        let error = load_or_create(&path, &Secrets::new()).expect_err("everyone can read it");
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
        let error =
            load_or_create(&path, &Secrets::new()).expect_err("group and others can read it");
        assert!(error.to_string().contains("mode 644"), "{error}");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
        let (_, provenance) = load_or_create(&path, &Secrets::new()).expect("user-only now");
        assert_eq!(provenance, Provenance::Existing);
        let _ = fs::remove_dir_all(&dir);
    }
}
