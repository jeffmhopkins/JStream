//! Code that runs on real-time threads: each input's audio thread (the
//! [`InputProducer`](input::InputProducer)) and the mixer thread ([`Mixer`](mixer::Mixer)).
//! Nothing in here allocates, locks, blocks or logs after construction (R-RT-001), and
//! the two sides exchange data only through the lock-free ring and atomics (R-RT-003).
//!
//! Docs: docs/ARCHITECTURE.md#threading, crates/jstream-audio/README.md#realtime

pub(crate) mod continuity;
pub(crate) mod control;
pub(crate) mod downmix;
pub(crate) mod drift;
pub(crate) mod input;
pub(crate) mod mixer;
pub(crate) mod ring;
pub(crate) mod shared;
pub(crate) mod sinc;
#[cfg(test)]
pub(crate) mod testutil;
