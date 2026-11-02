//! What can go wrong loading a pack. Every error names the file and, where there is one,
//! the effect and parameter, so a pack author can fix it without reading this crate.

use std::path::PathBuf;

/// Why a pack failed to load. A pack that fails is not loaded at all: no effect of a
/// broken pack becomes a kind.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PackError {
    /// A file could not be read.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The cause.
        #[source]
        source: std::io::Error,
    },
    /// A file is larger than this crate reads.
    #[error("{}: {size} bytes is larger than the {limit}-byte limit", path.display())]
    TooLarge {
        /// The file.
        path: PathBuf,
        /// Its size.
        size: u64,
        /// The limit for this kind of file.
        limit: u64,
    },
    /// `effect.toml` is not valid TOML, or doesn't have the manifest's shape.
    #[error("{}: {message}", path.display())]
    Syntax {
        /// The manifest.
        path: PathBuf,
        /// The parser's message, with its line and column.
        message: String,
    },
    /// The manifest parses but breaks a rule (an ID's form, a duplicate, a range).
    #[error("{}: {at}: {message}", path.display())]
    Manifest {
        /// The manifest.
        path: PathBuf,
        /// Where in it: `pack.id`, `effect.scanlines.param.strength.default`.
        at: String,
        /// What is wrong.
        message: String,
    },
    /// An effect's WGSL does not parse or validate against the host prelude, or breaks
    /// the effect contract (the `fs` entry point, no bindings of its own).
    #[error("{}: {message}", path.display())]
    Shader {
        /// The `.wgsl` file.
        path: PathBuf,
        /// What is wrong, with a line in the effect's own file when there is one.
        message: String,
    },
    /// Another pack folder already loaded a pack with this ID.
    #[error("{}: pack `{id}` is already loaded from {}", path.display(), first.display())]
    DuplicatePack {
        /// The manifest that lost.
        path: PathBuf,
        /// The pack ID.
        id: String,
        /// The folder that has it.
        first: PathBuf,
    },
    /// Loading the pack panicked: naga, parsing or validating its WGSL, is the part
    /// that can. The pack counts as one that failed to load.
    ///
    /// Only the payload's message is kept: a caught panic carries no location, and the
    /// apps' crash hook writes no report for the load thread. For a naga bug report,
    /// load the pack's folder with [`crate::Pack::load`] in a test, where the default
    /// panic hook runs: it prints the location, and `RUST_BACKTRACE=1` adds a backtrace.
    #[error("{}: loading the pack panicked: {message}", path.display())]
    Panicked {
        /// The pack's manifest.
        path: PathBuf,
        /// The panic's message, or `<not a string>` when its payload isn't one.
        message: String,
    },
}
