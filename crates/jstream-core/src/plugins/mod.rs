//! CLAP plugins hosted out of process (ADR-0035): the filter kinds a scan finds, and
//! the supervisor that keeps each filter's host process running, and a CLAP filter in
//! a source's audio chain.

pub(crate) mod filter;
pub mod kinds;
pub mod supervisor;
