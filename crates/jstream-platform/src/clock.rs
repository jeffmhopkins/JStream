//! The media clock (docs/adr/0014-media-clock.md).

use jstream_media::{MediaTime, Timebase};

/// The engine's one monotonic media clock (ADR-0014).
///
/// It is derived from the OS high-resolution counter (QPC on Windows), and no device is
/// its master. Every timestamp a backend hands the engine (capture frames, audio packets,
/// decoded frames) is already mapped onto this clock, so the engine compares and orders
/// them directly. On Windows that mapping is cheap, because Windows.Graphics.Capture,
/// Media Foundation and WASAPI already stamp their data in QPC units (spike S3 reads
/// WASAPI's `pu64QPCPosition` in 100 ns ticks).
///
/// Shared by every thread (`Arc<dyn MediaClock>`), so it is `Send + Sync`, and
/// [`now`](MediaClock::now) must be cheap and non-blocking: the audio and render threads
/// call it.
pub trait MediaClock: Send + Sync {
    /// The time base of every value [`now`](MediaClock::now) returns and of every
    /// timestamp this platform's backends produce. Fixed for the clock's lifetime.
    fn timebase(&self) -> Timebase;

    /// The current media time, in [`timebase`](MediaClock::timebase). Never decreases
    /// between calls, from any thread.
    fn now(&self) -> MediaTime;
}
