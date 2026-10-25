//! Spike S6: IOSurface and Metal GPU interop on macOS, and how to synchronise it.
//!
//! A producer (a second wgpu device, standing in for ScreenCaptureKit or a decoder) renders
//! into IOSurface-backed BGRA pixel buffers. The engine's wgpu device imports them through
//! wgpu-hal, converts them to NV12 in IOSurface-backed encode targets, and a consumer hands
//! those to VideoToolbox, with no CPU copy of frame data. Each frame carries its number in
//! two probe blocks, so the consumer can tell a frame that was read or handed over too early.
//! It runs that pipeline under each synchronisation choice (`--sync`). Throwaway code that
//! answers ADR-0063 decision 8; see `README.md` next to `Cargo.toml` for what it measures and
//! how to run it.
//!
//! Docs: docs/adr/0063-macos-platform-support.md, docs/adr/0004-wgpu-with-native-interop.md

#![warn(clippy::undocumented_unsafe_blocks)]

#[cfg(target_os = "macos")]
mod cli;
#[cfg(target_os = "macos")]
mod err;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
mod stats;

use std::process::ExitCode;

#[cfg(target_os = "macos")]
fn main() -> ExitCode {
    let args = match cli::Args::parse(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let mut failed = false;
    for mode in args.modes() {
        match mac::run(&args, mode) {
            Ok(report) => {
                if args.json {
                    println!("{}", report.to_json());
                } else {
                    println!("{}", report.to_text());
                }
            }
            Err(e) => {
                eprintln!("S6 failed ({}): {e}", mode.name());
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(not(target_os = "macos"))]
fn main() -> ExitCode {
    eprintln!("S6 runs on macOS only");
    ExitCode::FAILURE
}
