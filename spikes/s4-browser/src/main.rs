//! Spike S4's exe: the Windows run (CEF, wgpu, the audio handler). The options, test page
//! and checks it shares with the unit tests are in `lib.rs`.

#[cfg(windows)]
mod err;
#[cfg(windows)]
mod win;

#[cfg(windows)]
use s4_browser::cli;

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
