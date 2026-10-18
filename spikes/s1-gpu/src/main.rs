//! Spike S1: GPU interop on Windows.
//!
//! WGC capture -> shared D3D11 texture + shared fence -> wgpu (D3D12) -> NV12 (WGSL,
//! BT.709 limited range) -> Media Foundation hardware H.264 encoder, with no CPU copy of
//! frame data. Throwaway code that answers ROADMAP M0 / S1; see `README.md` next to
//! `Cargo.toml` for what it measures and how to run it.
//!
//! Docs: docs/adr/0004-wgpu-with-native-interop.md, docs/adr/0061-source-types-and-decoding-scope-v2.md

#![warn(clippy::undocumented_unsafe_blocks)]

#[cfg(windows)]
mod cli;
#[cfg(windows)]
mod err;
#[cfg(windows)]
mod stats;
#[cfg(windows)]
mod win;

use std::process::ExitCode;

#[cfg(windows)]
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
    match win::run(&args) {
        Ok(report) => {
            if args.json {
                println!("{}", report.to_json());
            } else {
                println!("{}", report.to_text());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("S1 failed: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("S1 runs on Windows only");
    ExitCode::FAILURE
}
