//! `jstream-cli`: drives a JStream engine from the command line. A client of the engine
//! through `jstream_control::Control` and `jstream-protocol` only (ADR-0001, R-ARCH-003):
//! queries, batches of edits, actions and the event stream, with human or `--json`
//! output and one exit code per failure kind. It reaches the engine over its WebSocket
//! API (`--url`, `--token-file`), or `jstream-control`'s in-memory mock (`--mock`, with
//! the `mock` cargo feature).
//!
//! Docs: docs/ARCHITECTURE.md#shape, docs/ARCHITECTURE.md#crate-map, apps/jstream-cli/README.md#purpose, apps/jstream-cli/README.md#usage, apps/jstream-cli/README.md#design, docs/adr/0001-library-first-engine.md

#![forbid(unsafe_code)]

mod args;
mod block;
mod exit;
mod run;

use std::io::{Write, stderr, stdout};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(arg) => {
                // Every argument is an ID, a name or a number: none can be non-UTF-8.
                // Best effort: the exit code reports the usage error either way.
                let _ = writeln!(
                    stderr().lock(),
                    "error: argument `{}` is not valid UTF-8",
                    arg.to_string_lossy()
                );
                return ExitCode::from(exit::USAGE);
            }
        }
    }
    let code = run::run(
        &args,
        &run::connect,
        &mut stdout().lock(),
        &mut stderr().lock(),
    );
    ExitCode::from(code)
}
