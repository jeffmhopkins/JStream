//! A page's audio: CEF's planar packets, or the browser helper's interleaved ones, handed
//! to the engine's mixer input as the interleaved packets an audio device delivers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};

use jstream_media::{AudioFormat, ChannelLayout, MediaTime, Rounding, SampleFormat, Timebase};
use jstream_platform::{
    AudioInputHandler, AudioPacket, AudioPacketFlags, AudioStream, PlatformError,
};

/// The rate pages' audio is asked for and delivered at: the mixer's own (ARCH ›
/// Threads), so nothing is resampled.
const RATE: u32 = 48_000;

/// The most frames handed on in one packet. CEF delivers what the backend asks for (10 ms,
/// 480 frames, in spike S4); a longer packet is handed on in pieces of this size.
pub const MAX_CHUNK_FRAMES: usize = 4096;

/// One page's audio, shared by the browser, which pushes packets, and the engine, which
/// records them into a mixer input ([`open`](Self::open)), the way the engine records an
/// audio device (`AudioIo::open_input`).
///
/// Packets arrive on CEF's audio thread. Handing one on takes the handler's lock with
/// `try_lock`, so the thread never waits: a packet that meets the lock taken (only while
/// the engine attaches or detaches its input) is dropped, and the next one's device
/// position shows the gap.
#[derive(Debug)]
pub struct PageAudio {
    sink: Mutex<Sink>,
    /// Frames pushed since the page opened, dropped ones included: the device position.
    position: AtomicU64,
    /// Packets dropped because the handler's lock was taken.
    dropped: AtomicU64,
}

struct Sink {
    handler: Option<Box<dyn AudioInputHandler>>,
    /// Which [`open`](PageAudio::open) the handler came from, so an old stream's drop
    /// doesn't detach a newer handler.
    generation: u64,
    /// Interleaved samples for one chunk, allocated once.
    scratch: Vec<f32>,
}

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sink")
            .field("attached", &self.handler.is_some())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl PageAudio {
    /// A page's audio, with no handler yet.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sink: Mutex::new(Sink {
                handler: None,
                generation: 0,
                scratch: vec![0.0; MAX_CHUNK_FRAMES * 2],
            }),
            position: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        })
    }

    /// The format packets are handed on in: 48 kHz stereo, interleaved `f32`. A page's
    /// mono audio is copied to both channels, and channels past the first two are
    /// dropped.
    pub fn format() -> AudioFormat {
        AudioFormat::new(RATE, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .expect("invariant: 48 kHz is a valid rate")
    }

    /// The sample rate the browser asks CEF for.
    pub fn rate() -> u32 {
        RATE
    }

    /// Starts handing packets to `handler`, in [`format`](Self::format), until the
    /// returned stream is dropped. A handler attached earlier is replaced, and dropped
    /// here.
    pub fn open(self: &Arc<Self>, handler: Box<dyn AudioInputHandler>) -> PageAudioStream {
        let old = {
            let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
            sink.generation += 1;
            let old = sink.handler.replace(handler);
            (old, sink.generation)
        };
        let (old, generation) = old;
        drop(old);
        PageAudioStream {
            audio: Arc::clone(self),
            generation,
        }
    }

    /// Packets dropped so far because the engine was attaching or detaching its input.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Hands on one packet of `frames` frames, in CEF's planar layout: one slice per
    /// channel. `arrived` is when it arrived, on the media clock; the packet is stamped as
    /// recorded that long before, which is when its first frame played in the page.
    ///
    /// Neither blocks nor allocates. A packet with no channels, or with a channel shorter
    /// than `frames`, is dropped as malformed.
    pub fn push_planar(&self, planes: &[&[f32]], frames: usize, arrived: Option<MediaTime>) {
        let (left, right) = match planes {
            [mono] => (*mono, *mono),
            [left, right, ..] => (*left, *right),
            [] => {
                self.position.fetch_add(frames as u64, Ordering::Relaxed);
                return;
            }
        };
        if left.len() < frames || right.len() < frames {
            self.position.fetch_add(frames as u64, Ordering::Relaxed);
            return;
        }
        self.push(frames, arrived, |i| (left[i], right[i]));
    }

    /// Hands on one packet of interleaved stereo samples, as the browser helper sends
    /// them; otherwise as [`push_planar`](Self::push_planar). A trailing odd sample is
    /// ignored.
    pub fn push_interleaved(&self, samples: &[f32], arrived: Option<MediaTime>) {
        let frames = samples.len() / 2;
        self.push(frames, arrived, |i| (samples[2 * i], samples[2 * i + 1]));
    }

    /// Hands on `frames` frames, frame `i` being `frame(i)`, in chunks of at most
    /// [`MAX_CHUNK_FRAMES`].
    fn push(&self, frames: usize, arrived: Option<MediaTime>, frame: impl Fn(usize) -> (f32, f32)) {
        let first = self.position.fetch_add(frames as u64, Ordering::Relaxed);
        if frames == 0 {
            return;
        }
        let mut sink = match self.sink.try_lock() {
            Ok(sink) => sink,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        let Sink {
            handler, scratch, ..
        } = &mut *sink;
        let Some(handler) = handler.as_mut() else {
            return;
        };
        let start = arrived.and_then(|t| t.checked_sub(frames_at(frames, t.timebase())?).ok());
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(MAX_CHUNK_FRAMES);
            let out = &mut scratch[..n * 2];
            for (i, pair) in out.chunks_exact_mut(2).enumerate() {
                (pair[0], pair[1]) = frame(done + i);
            }
            let time = start.and_then(|s| s.checked_add(frames_at(done, s.timebase())?).ok());
            handler.on_packet(&AudioPacket {
                samples: out,
                device_position: first + done as u64,
                time,
                flags: AudioPacketFlags::default(),
            });
            done += n;
        }
    }

    /// The page's audio failed and delivers nothing more until it plays again: the
    /// handler's stream ends ([`AudioInputHandler::on_error`]). Not for the error CEF
    /// reports while a page closes, which is the stream's normal end.
    pub fn fail(&self, error: &PlatformError) {
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(handler) = sink.handler.as_mut() {
            handler.on_error(error);
        }
    }
}

/// `frames` frames at 48 kHz in `timebase`, or `None` if that doesn't fit.
fn frames_at(frames: usize, timebase: Timebase) -> Option<MediaTime> {
    let rate = Timebase::per_second(RATE).ok()?;
    MediaTime::new(i64::try_from(frames).ok()?, rate)
        .rescale(timebase, Rounding::Nearest)
        .ok()
}

/// The engine's recording of a page's audio: packets go to its handler until it is
/// dropped.
#[derive(Debug)]
pub struct PageAudioStream {
    audio: Arc<PageAudio>,
    generation: u64,
}

impl AudioStream for PageAudioStream {
    fn format(&self) -> AudioFormat {
        PageAudio::format()
    }
}

impl Drop for PageAudioStream {
    /// Detaches the handler, unless a later [`PageAudio::open`] replaced it, and drops it
    /// here, after its last packet.
    fn drop(&mut self) {
        let handler = {
            let mut sink = self
                .audio
                .sink
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if sink.generation == self.generation {
                sink.handler.take()
            } else {
                None
            }
        };
        drop(handler);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what it is handed.
    #[derive(Default)]
    struct Recorded {
        packets: Vec<(Vec<f32>, u64, Option<MediaTime>)>,
        errors: usize,
    }

    struct Recorder(Arc<Mutex<Recorded>>);

    impl AudioInputHandler for Recorder {
        fn on_packet(&mut self, packet: &AudioPacket<'_>) {
            let mut r = self.0.lock().expect("lock");
            r.packets
                .push((packet.samples.to_vec(), packet.device_position, packet.time));
        }

        fn on_error(&mut self, _: &PlatformError) {
            self.0.lock().expect("lock").errors += 1;
        }
    }

    fn recorder() -> (Box<dyn AudioInputHandler>, Arc<Mutex<Recorded>>) {
        let shared = Arc::new(Mutex::new(Recorded::default()));
        (Box::new(Recorder(Arc::clone(&shared))), shared)
    }

    fn hns() -> Timebase {
        Timebase::new(1, 10_000_000).expect("100 ns ticks")
    }

    #[test]
    fn planar_packets_arrive_interleaved_and_stamped() {
        let audio = PageAudio::new();
        // Before anything is attached, packets only move the position.
        audio.push_planar(&[&[0.0; 4], &[0.0; 4]], 4, None);
        let (handler, seen) = recorder();
        let stream = audio.open(handler);
        assert_eq!(stream.format(), PageAudio::format());
        // 480 frames arriving at 1 s were recorded 10 ms before.
        let left: Vec<f32> = (0..480).map(|i| i as f32).collect();
        let right: Vec<f32> = (0..480).map(|i| -(i as f32)).collect();
        audio.push_planar(
            &[&left, &right],
            480,
            Some(MediaTime::new(10_000_000, hns())),
        );
        {
            let seen = seen.lock().expect("lock");
            let (samples, position, time) = &seen.packets[0];
            assert_eq!(&samples[..6], &[0.0, -0.0, 1.0, -1.0, 2.0, -2.0]);
            assert_eq!(samples.len(), 960);
            assert_eq!(*position, 4);
            assert_eq!(*time, Some(MediaTime::new(9_900_000, hns())));
        }
        // Mono goes to both channels.
        audio.push_planar(&[&[0.5; 2]], 2, None);
        assert_eq!(seen.lock().expect("lock").packets[1].0, vec![0.5; 4]);
        // Malformed packets are dropped.
        audio.push_planar(&[], 2, None);
        audio.push_planar(&[&[0.5; 1], &[0.5; 1]], 2, None);
        assert_eq!(seen.lock().expect("lock").packets.len(), 2);
        drop(stream);
        audio.push_planar(&[&[0.5; 2]], 2, None);
        assert_eq!(seen.lock().expect("lock").packets.len(), 2);
    }

    #[test]
    fn interleaved_packets_pass_through_and_are_stamped() {
        let audio = PageAudio::new();
        let (handler, seen) = recorder();
        let _stream = audio.open(handler);
        let samples: Vec<f32> = (0..960).map(|i| i as f32).collect();
        audio.push_interleaved(&samples, Some(MediaTime::new(10_000_000, hns())));
        // A trailing odd sample is ignored.
        audio.push_interleaved(&[1.0, 2.0, 3.0], None);
        let seen = seen.lock().expect("lock");
        assert_eq!(seen.packets[0].0, samples);
        assert_eq!(seen.packets[0].2, Some(MediaTime::new(9_900_000, hns())));
        assert_eq!(seen.packets[1].0, vec![1.0, 2.0]);
        assert_eq!(seen.packets[1].1, 480);
    }

    #[test]
    fn long_packets_are_handed_on_in_pieces() {
        let audio = PageAudio::new();
        let (handler, seen) = recorder();
        let _stream = audio.open(handler);
        let frames = MAX_CHUNK_FRAMES + 100;
        let plane = vec![0.25f32; frames];
        let arrived = MediaTime::new(48_000, Timebase::per_second(48_000).expect("rate"));
        audio.push_planar(&[&plane, &plane], frames, Some(arrived));
        let seen = seen.lock().expect("lock");
        assert_eq!(seen.packets.len(), 2);
        assert_eq!(seen.packets[0].0.len(), MAX_CHUNK_FRAMES * 2);
        assert_eq!(seen.packets[1].0.len(), 200);
        assert_eq!(seen.packets[1].1, MAX_CHUNK_FRAMES as u64);
        let start = 48_000 - frames as i64;
        assert_eq!(seen.packets[0].2.map(MediaTime::ticks), Some(start));
        assert_eq!(
            seen.packets[1].2.map(MediaTime::ticks),
            Some(start + MAX_CHUNK_FRAMES as i64)
        );
    }

    #[test]
    fn a_newer_handler_survives_an_older_streams_drop() {
        let audio = PageAudio::new();
        let (first, _) = recorder();
        let old = audio.open(first);
        let (second, seen) = recorder();
        let _new = audio.open(second);
        drop(old);
        audio.push_planar(&[&[0.1; 2]], 2, None);
        assert_eq!(seen.lock().expect("lock").packets.len(), 1);
        audio.fail(&PlatformError::DeviceLost {
            what: "page audio".into(),
            reason: "test".into(),
        });
        assert_eq!(seen.lock().expect("lock").errors, 1);
    }

    #[test]
    fn a_packet_meeting_the_lock_is_dropped_and_counted() {
        let audio = PageAudio::new();
        let (handler, seen) = recorder();
        let _stream = audio.open(handler);
        let held = audio.sink.lock().expect("lock");
        audio.push_planar(&[&[0.1; 2]], 2, None);
        drop(held);
        audio.push_planar(&[&[0.1; 2]], 2, None);
        assert_eq!(audio.dropped(), 1);
        let seen = seen.lock().expect("lock");
        assert_eq!(seen.packets.len(), 1);
        // The gap shows in the position.
        assert_eq!(seen.packets[0].1, 2);
    }
}
