//! The engine's load for `stats.tick` (protocol 0.48): when the engine started, the
//! process CPU time the host lets it read, and the idle heartbeat.
//!
//! The program frames come from the [`RenderHub`](crate::runtime::RenderHub), which the
//! render thread counts into; this module adds the clock and the CPU time beside them.
//! While an output runs, the program stage's `jstream-stats` thread emits the ticks.
//! While none runs, the heartbeat a host starts ([`Engine::start_stats_heartbeat`])
//! emits one about once a second, so a client sees the engine's CPU use when idle too.
//!
//! [`Engine::start_stats_heartbeat`]: crate::Engine::start_stats_heartbeat

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jstream_platform::ProcessCpu;
use jstream_protocol::EngineLoad;

use crate::lock;

/// What a `stats.tick`'s load reads besides the render hub.
pub(crate) struct LoadMeter {
    started: Instant,
    cpu: Mutex<Option<Arc<dyn ProcessCpu>>>,
    cpus: u32,
    /// How many program stages' `jstream-stats` threads run: while any does, the
    /// heartbeat leaves the ticks to it.
    tickers: AtomicUsize,
    /// Whether a heartbeat thread was started, so a second start does nothing.
    heartbeat: AtomicBool,
}

impl Default for LoadMeter {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism()
            .ok()
            .and_then(|n| u32::try_from(n.get()).ok())
            .unwrap_or(0);
        Self {
            started: Instant::now(),
            cpu: Mutex::new(None),
            cpus,
            tickers: AtomicUsize::new(0),
            heartbeat: AtomicBool::new(false),
        }
    }
}

impl LoadMeter {
    /// Reads the process CPU time from `cpu` from now on.
    pub(crate) fn set_cpu(&self, cpu: Arc<dyn ProcessCpu>) {
        *lock(&self.cpu) = Some(cpu);
    }

    /// The process CPU time, or `None` with no reader or when the OS refused. Takes
    /// only this meter's own lock, to clone the reader: the call runs outside it.
    pub(crate) fn cpu_time(&self) -> Option<Duration> {
        let cpu = lock(&self.cpu).clone()?;
        cpu.cpu_time()
    }

    /// The load with `program_frames` and the CPU time `cpu`, read now.
    pub(crate) fn load(&self, program_frames: u64, cpu: Option<Duration>) -> EngineLoad {
        EngineLoad {
            elapsed_ms: millis(self.started.elapsed()),
            program_frames,
            cpu_ms: cpu.map(millis),
            cpus: self.cpus,
        }
    }

    /// A program stage's `jstream-stats` thread started.
    pub(crate) fn ticker_started(&self) {
        self.tickers.fetch_add(1, Ordering::AcqRel);
    }

    /// A program stage's `jstream-stats` thread ended.
    pub(crate) fn ticker_ended(&self) {
        let _ = self
            .tickers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
    }

    /// Whether a program stage's `jstream-stats` thread runs.
    pub(crate) fn ticking(&self) -> bool {
        self.tickers.load(Ordering::Acquire) > 0
    }

    /// Claims the one heartbeat: `true` for the first caller only.
    pub(crate) fn claim_heartbeat(&self) -> bool {
        !self.heartbeat.swap(true, Ordering::AcqRel)
    }
}

/// `duration` in whole milliseconds, saturating.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::LoadMeter;
    use jstream_platform::fake::FakeCpu;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn the_load_reads_the_hosts_cpu_time_in_milliseconds() {
        let meter = LoadMeter::default();
        assert_eq!(meter.cpu_time(), None, "no reader yet");
        let cpu = FakeCpu::new();
        meter.set_cpu(Arc::new(cpu.clone()));
        cpu.set(Some(Duration::from_micros(1_500_900)));
        let load = meter.load(42, meter.cpu_time());
        assert_eq!(load.program_frames, 42);
        assert_eq!(load.cpu_ms, Some(1_500));
        assert!(load.cpus >= 1, "this machine has a CPU");
        cpu.set(None);
        assert_eq!(meter.load(42, meter.cpu_time()).cpu_ms, None);
    }

    #[test]
    fn the_heartbeat_is_claimed_once_and_tickers_are_counted() {
        let meter = LoadMeter::default();
        assert!(meter.claim_heartbeat());
        assert!(!meter.claim_heartbeat());
        assert!(!meter.ticking());
        meter.ticker_started();
        meter.ticker_started();
        meter.ticker_ended();
        assert!(meter.ticking(), "one stage still ticks");
        meter.ticker_ended();
        meter.ticker_ended();
        assert!(!meter.ticking(), "an extra end does not wrap");
    }
}
