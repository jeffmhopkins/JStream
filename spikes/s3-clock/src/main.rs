//! Spike S3: the media clock and audio drift on Windows.
//!
//! WASAPI mic + default-render loopback (or one process's loopback), event-driven,
//! shared mode. Every packet's device position and QPC timestamp is mapped onto the QPC
//! media clock; each device's drift against QPC is fitted over the run; each stream is
//! resampled adaptively onto a QPC-clocked engine rate with a PI-steered rubato sinc
//! resampler, and the residual timing error is measured. Throwaway code that answers
//! ROADMAP M0 / S3; see `README.md` next to `Cargo.toml` for what it measures and how
//! to run it.
//!
//! The per-packet logic (`clock`, `drift`, `control`, `track`, `stream`) is
//! platform-independent and unit-tested on any OS; only `win` touches WASAPI.
//!
//! Docs: docs/adr/0014-media-clock.md, docs/adr/0008-windows-support-policy.md

#![warn(clippy::undocumented_unsafe_blocks)]

// The measurement logic is built on Windows, where `win` drives it, and for the tests
// on any OS. Elsewhere the binary is only the stub below.
#[cfg(any(windows, test))]
mod cli;
#[cfg(any(windows, test))]
mod clock;
#[cfg(any(windows, test))]
mod control;
#[cfg(any(windows, test))]
mod drift;
#[cfg(windows)]
mod err;
#[cfg(any(windows, test))]
mod report;
#[cfg(any(windows, test))]
mod stream;
#[cfg(test)]
mod testutil;
#[cfg(any(windows, test))]
mod track;
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
            eprintln!("S3 failed: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("S3 runs on Windows only (WASAPI); `cargo test` runs its platform-independent logic");
    ExitCode::FAILURE
}
