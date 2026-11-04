//! The control token file (ADR-0006), through `jstream_server::token_file`, which the
//! desktop app's `--mcp` shares: the daemon reuses the token in an existing file, so
//! clients configured with it keep working across restarts, and otherwise creates the
//! file with a new random token.

use jstream_crash::Secrets;
use jstream_server::Token;
pub use jstream_server::token_file::{Provenance, device_file};
use std::path::Path;

/// Reads the token in `path`, or creates the file with a new token
/// (`jstream_server::token_file::load_or_create`, which says what an existing file must
/// be), and registers it with the crash reports' redaction (`secrets`).
pub fn load_or_create(path: &Path, secrets: &Secrets) -> anyhow::Result<(Token, Provenance)> {
    let loaded = jstream_server::token_file::load_or_create(path)?;
    secrets.register(&loaded.text);
    Ok((loaded.token, loaded.provenance))
}

#[cfg(test)]
mod tests {
    use super::{Provenance, load_or_create};
    use jstream_crash::Secrets;
    use std::fs;

    /// The file's checks are `jstream_server::token_file`'s, tested there; the daemon's
    /// part is the redaction of a created and of a reused token.
    #[test]
    fn created_and_reused_tokens_are_redacted() {
        let dir = std::env::temp_dir().join(format!(
            "jstream-daemon-token-{}-redact",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the scratch directory is created");
        let path = dir.join("token");
        let created = Secrets::new();
        let (_, provenance) = load_or_create(&path, &created).expect("created");
        assert_eq!(provenance, Provenance::Created);
        let written = fs::read_to_string(&path).expect("readable");
        assert!(
            created.is_registered(written.trim()),
            "a new token is redacted from crash reports"
        );
        let reused = Secrets::new();
        let (_, provenance) = load_or_create(&path, &reused).expect("reused");
        assert_eq!(provenance, Provenance::Existing);
        assert!(
            reused.is_registered(written.trim()),
            "a reused token is redacted from crash reports"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
