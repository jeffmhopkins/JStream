//! CLAP plugins hosted out of process (ADR-0035): the filter kinds a scan finds, and
//! the supervisor that keeps each filter's host process running.

pub mod kinds;
pub mod supervisor;
