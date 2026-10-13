//! Spike S2: the producer exe and registration tool for the JStream S2 virtual camera.
//!
//! `--register hkcu|hklm`, `--unregister` and `--status` manage the COM registration of
//! `s2_vcam.dll` (next to this exe). With no mode, it creates the shared textures, fence
//! and control block, renders a moving test pattern into them, creates the virtual camera
//! with `MFCreateVirtualCamera` and reports what happened. See `README.md` next to
//! `Cargo.toml`.
//!
//! Docs: docs/adr/0020-portable-windows-distribution.md

#![warn(clippy::undocumented_unsafe_blocks)]

#[cfg(windows)]
mod cli;
#[cfg(windows)]
mod err;
#[cfg(windows)]
mod json;
#[cfg(windows)]
mod producer;
#[cfg(windows)]
mod run;
#[cfg(windows)]
mod sys;

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
    match run::main(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("S2 failed: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("S2 runs on Windows only (Windows 11 build 22000 or later)");
    ExitCode::FAILURE
}
