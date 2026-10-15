//! Spike S4: CEF browser source on Windows.
//!
//! CEF off-screen rendering with accelerated paint -> the shared D3D11 texture opened on
//! wgpu's D3D12 device -> copied out of CEF's pool and composited over a background, with
//! no CPU copy of frame data; plus CEF's per-browser audio handler. Throwaway code that
//! answers ADR-0021's "prove it first"; see `README.md` next to `Cargo.toml`.
//!
//! Docs: docs/adr/0021-browser-source.md, docs/adr/0004-wgpu-with-native-interop.md

// Off Windows only the unit tests of the platform-independent modules build.
#![cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "the Windows run uses these; elsewhere only tests do"
    )
)]

#[cfg(any(windows, test))]
mod analysis;
#[cfg(any(windows, test))]
mod cli;
#[cfg(windows)]
mod err;
#[cfg(any(windows, test))]
mod json;
#[cfg(any(windows, test))]
mod page;
#[cfg(any(windows, test))]
mod stats;
#[cfg(windows)]
mod win;

use std::process::ExitCode;

#[cfg(windows)]
fn main() -> ExitCode {
    // In the same-exe layout CEF starts its subprocesses as this exe; they stop here.
    if let Some(code) = win::subprocess() {
        std::process::exit(code);
    }
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
    let (report, ok) = match win::run(&args) {
        Ok(report) => (report, true),
        Err(e) => {
            eprintln!("S4 failed: {e}");
            (win::Report::from(e), false)
        }
    };
    if args.json {
        println!("{}", report.json);
    } else {
        print!("{}", report.text);
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("S4 runs on Windows only");
    ExitCode::FAILURE
}
