//! `cargo xtask`: repository automation.
//!
//! Docs: docs/ARCHITECTURE.md#repo-layout, docs/DOCS.md#bootstrap

#![forbid(unsafe_code)]
#![allow(
    clippy::print_stderr,
    reason = "xtask is a command-line tool; stderr is its UI"
)]

mod docs;

use std::path::Path;
use std::process::ExitCode;

/// Subcommands planned for M0 (ROADMAP.md#m0) that are not implemented yet.
const PLANNED: &[(&str, &str)] = &[
    (
        "lint-deps",
        "crate layering, unsafe declarations, links= allowlist (R-ARCH-002)",
    ),
    (
        "governance",
        "label + justification for governance changes (R-GOV-001)",
    ),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    match args.first().map(String::as_str) {
        Some("docs") => match docs::run(&args[1..], &root) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("xtask docs: {e:#}");
                ExitCode::from(2)
            }
        },
        Some(name) if PLANNED.iter().any(|(n, _)| *n == name) => {
            eprintln!("xtask {name}: not implemented yet (planned for M0)");
            ExitCode::from(2)
        }
        _ => {
            eprintln!("usage: cargo xtask <command>\n\ncommands:");
            eprintln!(
                "  {:<12} check the documentation graph (docs/DOCS.md)",
                "docs check"
            );
            for (name, what) in PLANNED {
                eprintln!("  {name:<12} {what} [planned]");
            }
            ExitCode::from(2)
        }
    }
}
