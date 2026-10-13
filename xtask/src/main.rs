//! `cargo xtask`: repository automation.
//!
//! Docs: docs/ARCHITECTURE.md#repo-layout

#![forbid(unsafe_code)]
#![allow(
    clippy::print_stderr,
    reason = "xtask is a command-line tool; stderr is its UI"
)]

use std::process::ExitCode;

/// Subcommands planned for M0 (ROADMAP.md#m0). Each is implemented in its own PR.
const PLANNED: &[(&str, &str)] = &[
    (
        "docs",
        "check/verify/for/log the documentation graph (docs/DOCS.md)",
    ),
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
    let cmd = std::env::args().nth(1);
    match cmd.as_deref() {
        Some(name) if PLANNED.iter().any(|(n, _)| *n == name) => {
            eprintln!("xtask {name}: not implemented yet (planned for M0)");
            ExitCode::from(2)
        }
        _ => {
            eprintln!("usage: cargo xtask <command>\n\nplanned commands:");
            for (name, what) in PLANNED {
                eprintln!("  {name:<12} {what}");
            }
            ExitCode::from(2)
        }
    }
}
