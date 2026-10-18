//! `jstream-plugin-host`: the out-of-process CLAP plugin host (ADR-0035). The engine
//! starts one per CLAP filter instance, so a plugin that crashes or hangs costs only its
//! own filter. It is the only program that links `jstream-plugin-clap`, so third-party
//! native code never loads into the engine's process.
//!
//! - `jstream-plugin-host scan <file>` writes one `Plugins` control message (the file's
//!   hostable audio effects and their parameters) to stdout and exits.
//! - `jstream-plugin-host run <block> <file> <plugin-id>` opens the shared block named
//!   `<block>` the engine created, and runs that plugin over it (`session`) on a
//!   real-time thread registered with MMCSS (`rt`), with control messages on stdin and
//!   stdout, until the engine stops it. Windows only, for now.
//!
//! Exit status: 0 after a clean run or scan, 1 on a failure (also reported on stdout as
//! an `Error` message, and on the block), 2 for a usage error.
//!
//! Docs: apps/jstream-plugin-host/README.md#purpose, docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#shape, docs/adr/0035-clap-plugins-out-of-process.md

#![forbid(unsafe_code)]

// `run` hosts plugins on Windows only for now; elsewhere these two are only tested.
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "used by `run`, which is Windows-only for now")
)]
mod rt;
mod scan;
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "used by `run`, which is Windows-only for now")
)]
mod session;

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use jstream_plugin_clap::Library;
use jstream_plugin_proto::control::FromHost;

use crate::session::Outbox;

// Counts allocations inside `assert_no_alloc` scopes in the tests (R-RT-001's check for
// the real-time loop).
#[cfg(test)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const USAGE: &str = "\
usage: jstream-plugin-host scan <plugin-file>
       jstream-plugin-host run <block-name> <plugin-file> <plugin-id>
       jstream-plugin-host --version

The engine starts this program; it isn't meant to be run by hand.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["scan", file] => scan(Path::new(file)),
        ["run", block, file, id] => run(block, Path::new(file), id),
        ["--version"] => {
            let _ = writeln!(
                std::io::stdout(),
                "jstream-plugin-host {}",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        _ => {
            let _ = std::io::stderr().write_all(USAGE.as_bytes());
            ExitCode::from(2)
        }
    }
}

fn scan(file: &Path) -> ExitCode {
    let mut out = Outbox(std::io::stdout().lock());
    match Library::load(file) {
        Ok(library) => {
            out.send(&FromHost::Plugins(scan::scan(&library)));
            ExitCode::SUCCESS
        }
        Err(e) => {
            out.send(&FromHost::Error(e.to_string()));
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn run(block: &str, file: &Path, id: &str) -> ExitCode {
    use jstream_platform_windows::{PluginBlock, ProAudioThread};
    use jstream_plugin_proto::{ERROR_LOAD, STATE_FAILED};
    use std::sync::atomic::Ordering;

    let mut out = Outbox(std::io::stdout().lock());
    let block = match PluginBlock::open(block) {
        Ok(block) => block,
        Err(e) => {
            out.send(&FromHost::Error(format!("opening the shared block: {e}")));
            return ExitCode::FAILURE;
        }
    };
    let shared = block.as_ref();
    let library = match Library::load(file) {
        Ok(library) => library,
        Err(e) => {
            shared
                .status
                .host_error
                .store(ERROR_LOAD, Ordering::Relaxed);
            shared
                .status
                .host_state
                .store(STATE_FAILED, Ordering::Release);
            out.send(&FromHost::Error(e.to_string()));
            return ExitCode::FAILURE;
        }
    };
    match session::run(
        shared,
        &library,
        id,
        std::io::stdin(),
        out.0,
        ProAudioThread::register,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

#[cfg(not(windows))]
fn run(_block: &str, _file: &Path, _id: &str) -> ExitCode {
    Outbox(std::io::stdout().lock()).send(&FromHost::Error(
        "the plugin host runs plugins on Windows only, for now".into(),
    ));
    ExitCode::FAILURE
}
