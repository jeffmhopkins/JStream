//! The MIDI the visualization sources draw (ADR-0137): [`MidiState`], a lock-free record
//! of one input's held keys, controllers and recent notes, [`MidiInputs`], the lookup
//! that finds a source's input (one by name, or every input at once), and the source
//! machinery the `midi_keyboard`, `midi_piano_roll`, `midi_pads` and `midi_cc` kinds
//! share.
//!
//! Whoever reads MIDI (an input's thread) calls [`MidiInputs::push`] with each message
//! and its time on the media clock. A source reads the state when its [`Painter`] draws
//! a frame, on the thread that paints, so the drawing never runs on an input's thread,
//! and a push neither locks nor allocates once the input is known
//! ([R-RT-003](../../../../docs/RULES.md#R-RT-003)). `Source::frame` only hands out the
//! frame painted last, as the scope sources do.

mod kinds;
mod tempo;

pub(crate) use kinds::{BeatKind, CcKind, KeyboardKind, PadsKind, PianoRollKind};
pub use tempo::{CLOCK_TIMEOUT_MS, MidiFeed, TempoReading};

use crate::color::{parse_color, rgba_frame};
use crate::source::{at_millis, changed_non_live, millis, setting};
use crate::{Context, Limits, Painter, Source, SourceError};
use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
use jstream_midi::Message;
use jstream_platform::MediaClock;
use jstream_protocol::{KindDescriptor, Settings};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI16, AtomicU8, AtomicU64, Ordering, fence};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

/// MIDI's channels.
pub const CHANNELS: usize = 16;
/// MIDI's keys, and its controllers.
pub const KEYS: usize = 128;
/// Note events the log keeps: at 1000 notes a second, a second of them, more than any
/// piano roll shows of a real performance.
pub(crate) const LOG: usize = 1 << 10;
/// How often a MIDI source draws a new frame.
const FPS: u64 = 30;
/// How often a source with nothing moving checks whether its input moves again.
const STILL_MS: u64 = 250;
/// The longest input name, as `Trigger`'s `input` (ADR-0122).
pub const MAX_INPUT_NAME: usize = jstream_protocol::MAX_INPUT_NAME_LEN;
/// The most inputs [`MidiInputs`] tracks by name; a push from one more feeds only
/// the any-input state.
pub const MAX_INPUTS: usize = 64;

/// One note event as the log keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteEvent {
    /// When it happened, in milliseconds on the media clock.
    pub at_ms: u64,
    /// The channel, 0 to 15.
    pub channel: u8,
    /// The key, 0 to 127.
    pub key: u8,
    /// The velocity, 1 to 127 for a note-on; 0 for a note-off.
    pub velocity: u8,
}

impl NoteEvent {
    /// Packs the event into one word: 40 bits of milliseconds (35 years), the
    /// channel, key and velocity.
    fn pack(self) -> u64 {
        (self.at_ms & ((1 << 40) - 1)) << 24
            | u64::from(self.channel & 0x0F) << 16
            | u64::from(self.key & 0x7F) << 8
            | u64::from(self.velocity & 0x7F)
    }

    fn unpack(word: u64) -> Self {
        Self {
            at_ms: word >> 24,
            channel: (word >> 16) as u8 & 0x0F,
            key: (word >> 8) as u8 & 0x7F,
            velocity: word as u8 & 0x7F,
        }
    }
}

/// One slot of the note log: the event, and which push wrote it.
struct Slot {
    /// One more than the push's index, so 0 is a slot never written; 0 again while a
    /// push writes it.
    seq: AtomicU64,
    event: AtomicU64,
}

/// What one MIDI input (or every input) is doing: each channel's held keys with their
/// velocity, its controllers, pitch bend and channel pressure, and a log of the latest
/// note events. Any number of threads may push and read: every field is an atomic, so
/// nothing locks or allocates, and a reader sees each value whole.
pub struct MidiState {
    /// Held velocity by channel and key; 0 is up.
    keys: Box<[AtomicU8]>,
    /// Controller values by channel and controller.
    controllers: Box<[AtomicU8]>,
    /// Pitch bend by channel, -8192 to 8191.
    bend: [AtomicI16; CHANNELS],
    /// Channel pressure by channel.
    pressure: [AtomicU8; CHANNELS],
    log: Box<[Slot]>,
    /// Note events pushed: event `n` is in slot `n % LOG`.
    pushed: AtomicU64,
    /// When the last message arrived, in milliseconds, plus one; 0 before any.
    last_ms: AtomicU64,
    /// Messages pushed, of every kind: what a settled source compares to see whether
    /// anything changed.
    changes: AtomicU64,
    /// The tempo the input's clock gives, which its [`MidiFeed`] publishes.
    tempo: tempo::TempoCell,
}

impl std::fmt::Debug for MidiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidiState")
            .field("changes", &self.changes())
            .finish_non_exhaustive()
    }
}

impl Default for MidiState {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiState {
    /// A state with nothing held, every controller at 0 and no notes logged.
    pub fn new() -> Self {
        let atomics = |n| (0..n).map(|_| AtomicU8::new(0)).collect();
        Self {
            keys: atomics(CHANNELS * KEYS),
            controllers: atomics(CHANNELS * KEYS),
            bend: std::array::from_fn(|_| AtomicI16::new(0)),
            pressure: std::array::from_fn(|_| AtomicU8::new(0)),
            log: (0..LOG)
                .map(|_| Slot {
                    seq: AtomicU64::new(0),
                    event: AtomicU64::new(0),
                })
                .collect(),
            pushed: AtomicU64::new(0),
            last_ms: AtomicU64::new(0),
            changes: AtomicU64::new(0),
            tempo: tempo::TempoCell::new(),
        }
    }

    /// Records `message`, which arrived at `at_ms` on the media clock. Messages that
    /// change nothing drawn (clock, SysEx, time code) only count as activity.
    pub fn push(&self, at_ms: u64, message: &Message) {
        self.last_ms
            .fetch_max(at_ms.saturating_add(1), Ordering::Relaxed);
        match *message {
            Message::NoteOn {
                channel,
                key,
                velocity,
            } => self.note(at_ms, channel.wire(), key, velocity),
            Message::NoteOff { channel, key, .. } => self.note(at_ms, channel.wire(), key, 0),
            Message::ControlChange {
                channel,
                controller,
                value,
            } => {
                let channel = channel.wire();
                self.controllers[index(channel, controller)].store(value, Ordering::Relaxed);
                // All notes off (123), and all sound off (120), release every key.
                if matches!(controller, 120 | 123) {
                    for key in 0..KEYS as u8 {
                        if self.held(channel, key) > 0 {
                            self.note(at_ms, channel, key, 0);
                        }
                    }
                }
            }
            Message::PitchBend { channel, value } => {
                self.bend[usize::from(channel.wire())].store(value, Ordering::Relaxed);
            }
            Message::ChannelPressure { channel, pressure } => {
                self.pressure[usize::from(channel.wire())].store(pressure, Ordering::Relaxed);
            }
            _ => {}
        }
        // After every store above, so a reader that sees the count sees what it counts.
        self.changes.fetch_add(1, Ordering::Release);
    }

    fn note(&self, at_ms: u64, channel: u8, key: u8, velocity: u8) {
        let key = key & 0x7F;
        self.keys[index(channel, key)].store(velocity, Ordering::Relaxed);
        let n = self.pushed.fetch_add(1, Ordering::Relaxed);
        let slot = &self.log[(n % LOG as u64) as usize];
        slot.seq.store(0, Ordering::Relaxed);
        // Keeps the event store below from being seen before the slot is marked as
        // being written.
        fence(Ordering::Release);
        let event = NoteEvent {
            at_ms,
            channel,
            key,
            velocity,
        };
        slot.event.store(event.pack(), Ordering::Relaxed);
        slot.seq.store(n + 1, Ordering::Release);
    }

    /// The tempo the input's MIDI clock gave last, from its [`MidiFeed`]; `None` before
    /// it has sent clock or transport.
    pub fn tempo(&self) -> Option<TempoReading> {
        self.tempo.read()
    }

    /// The velocity `key` is held at on `channel` (0 to 15), or 0 when it is up.
    pub fn held(&self, channel: u8, key: u8) -> u8 {
        self.keys[index(channel, key)].load(Ordering::Relaxed)
    }

    /// The value of `controller` on `channel`.
    pub fn controller(&self, channel: u8, controller: u8) -> u8 {
        self.controllers[index(channel, controller)].load(Ordering::Relaxed)
    }

    /// The pitch bend on `channel`, -8192 to 8191.
    pub fn bend(&self, channel: u8) -> i16 {
        self.bend[usize::from(channel & 0x0F)].load(Ordering::Relaxed)
    }

    /// The channel pressure on `channel`.
    pub fn pressure(&self, channel: u8) -> u8 {
        self.pressure[usize::from(channel & 0x0F)].load(Ordering::Relaxed)
    }

    /// When the last message arrived, in milliseconds, or `None` before any.
    pub fn last_ms(&self) -> Option<u64> {
        self.last_ms.load(Ordering::Relaxed).checked_sub(1)
    }

    /// Messages pushed so far, of every kind, whether or not they changed anything.
    pub fn changes(&self) -> u64 {
        self.changes.load(Ordering::Acquire)
    }

    /// Note events pushed so far.
    pub fn pushed(&self) -> u64 {
        self.pushed.load(Ordering::Acquire)
    }

    /// Fills `out` with the latest note events, oldest first, up to its capacity (at
    /// most the log's 1024), skipping any a push is writing at that moment. Allocates
    /// only if `out` must grow.
    pub fn recent(&self, out: &mut Vec<NoteEvent>, most: usize) {
        out.clear();
        let end = self.pushed();
        let start = end.saturating_sub(most.min(LOG) as u64);
        for n in start..end {
            let slot = &self.log[(n % LOG as u64) as usize];
            if slot.seq.load(Ordering::Acquire) != n + 1 {
                continue;
            }
            let word = slot.event.load(Ordering::Relaxed);
            // Keeps the second `seq` load from being seen before the event load.
            fence(Ordering::Acquire);
            // Overwritten while it was read: skip it rather than show a mix.
            if slot.seq.load(Ordering::Relaxed) == n + 1 {
                out.push(NoteEvent::unpack(word));
            }
        }
    }
}

/// The index of `key` (or a controller) on `channel` in the per-key tables.
fn index(channel: u8, key: u8) -> usize {
    usize::from(channel & 0x0F) * KEYS + usize::from(key & 0x7F)
}

/// Every MIDI input's state by name, and one for all of them together: what a MIDI
/// source's `input` setting names. Cloning it shares it.
#[derive(Clone, Default)]
pub struct MidiInputs {
    any: Arc<MidiState>,
    named: Arc<Mutex<BTreeMap<String, Arc<MidiState>>>>,
    /// The media clock messages are stamped on, which the sources draw against: set
    /// once, when the host has one, and shared by every clone and every source.
    clock: Arc<OnceLock<Arc<dyn MediaClock>>>,
}

impl std::fmt::Debug for MidiInputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidiInputs")
            .field("inputs", &lock(&self.named).keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl MidiInputs {
    /// No inputs yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stamps the inputs on `clock` from now on: the sources that draw them, made
    /// before or after, read the time from it, so a note's age is measured on the
    /// clock its `at_ms` is on. Without one (tests), a source measures it on the time
    /// it is painted for. Only the first clock set counts; it returns whether this was
    /// it.
    pub fn set_clock(&self, clock: Arc<dyn MediaClock>) -> bool {
        self.clock.set(clock).is_ok()
    }

    /// The media clock's time now, in milliseconds, once it has one.
    pub fn now_ms(&self) -> Option<u64> {
        self.clock.get().map(|clock| clock_ms(clock.as_ref()))
    }

    /// The state every input feeds.
    pub fn any(&self) -> Arc<MidiState> {
        Arc::clone(&self.any)
    }

    /// The state of the input `name`, made empty the first time it is asked for, or
    /// `None` for an empty or too-long name, or once [`MAX_INPUTS`] are known.
    pub fn input(&self, name: &str) -> Option<Arc<MidiState>> {
        if name.is_empty() || name.len() > MAX_INPUT_NAME {
            return None;
        }
        let mut named = lock(&self.named);
        if let Some(state) = named.get(name) {
            return Some(Arc::clone(state));
        }
        if named.len() >= MAX_INPUTS {
            return None;
        }
        let state = Arc::new(MidiState::new());
        named.insert(name.to_owned(), Arc::clone(&state));
        Some(state)
    }

    /// The state a source whose `input` setting is `name` draws: every input's for an
    /// empty name.
    pub fn drawn(&self, name: &str) -> Arc<MidiState> {
        if name.is_empty() {
            return self.any();
        }
        self.input(name)
            .unwrap_or_else(|| Arc::new(MidiState::new()))
    }

    /// Records `message` from input `name` at `at_ms`, in that input's state and the
    /// any-input state. An input thread looks its state up once with
    /// [`input`](Self::input) and pushes to it and to [`any`](Self::any) itself, so
    /// it never takes this lookup's lock per message; this is for callers that don't.
    pub fn push(&self, name: &str, at_ms: u64, message: &Message) {
        if let Some(state) = self.input(name) {
            state.push(at_ms, message);
        }
        self.any.push(at_ms, message);
    }
}

/// What a MIDI kind draws; [`MidiSource`] does the rest.
trait Draw: Send + 'static {
    /// The live settings.
    type Style: Clone + Send + 'static;

    /// Reads the live settings.
    fn style(settings: &Settings) -> Result<Self::Style, SourceError>;

    /// Applies new live settings.
    fn restyle(&mut self, style: Self::Style);

    /// Draws the state at `now_ms` into `canvas`. Returns whether anything drawn is
    /// still moving, so the next tick would draw something else even with no new MIDI.
    fn draw(&mut self, state: &MidiState, now_ms: u64, canvas: &mut Canvas<'_>) -> bool;
}

/// A running MIDI source. Its [`Painter`] draws each frame; `frame` only hands out the
/// one painted last.
struct MidiSource<D: Draw> {
    descriptor: KindDescriptor,
    settings: Settings,
    painter: Arc<MidiPainter<D>>,
}

/// Builds a MIDI source of `draw` and paints its first frame.
fn create<D: Draw>(
    descriptor: &KindDescriptor,
    settings: &Settings,
    ctx: &Context,
    draw: D,
) -> Result<Box<dyn Source>, SourceError> {
    let size = frame_size(settings, &ctx.limits)?;
    let name = input(settings)?;
    let state = ctx
        .midi
        .as_ref()
        .map_or_else(|| Arc::new(MidiState::new()), |inputs| inputs.drawn(name));
    let mut drawing = Drawing {
        draw,
        state,
        clock: ctx
            .midi
            .as_ref()
            .map_or_else(Arc::default, |inputs| Arc::clone(&inputs.clock)),
        frames: Frames::new(size)?,
        drawn: None,
        seen: None,
        moving: true,
    };
    drawing.paint(MediaTime::zero(Timebase::MILLIS));
    let ready = Ready {
        frame: drawing.frames.current(),
        settled: false,
        restyle: None,
    };
    Ok(Box::new(MidiSource {
        descriptor: descriptor.clone(),
        settings: settings.clone(),
        painter: Arc::new(MidiPainter {
            fed: ctx.midi.is_some(),
            drawing: Mutex::new(drawing),
            ready: Mutex::new(ready),
        }),
    }))
}

impl<D: Draw> Source for MidiSource<D> {
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Ok(self.painter.frame())
    }

    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        self.painter.next_change(at)
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        if let Some(param) = changed_non_live(&self.descriptor, &self.settings, settings) {
            return Err(SourceError::NotLive {
                param: param.to_owned(),
            });
        }
        let style = D::style(settings)?;
        let mut ready = lock(&self.painter.ready);
        ready.restyle = Some(style);
        ready.settled = false;
        drop(ready);
        self.settings = settings.clone();
        Ok(())
    }

    fn painter(&self) -> Option<Arc<dyn Painter>> {
        let painter: Arc<dyn Painter> = self.painter.clone();
        Some(painter)
    }
}

/// A MIDI source's drawing, shared between the source and whoever paints it.
struct MidiPainter<D: Draw> {
    /// Whether there is a MIDI input to read: without one, the first frame never changes.
    fed: bool,
    /// Held while a frame is drawn. Only [`Painter::paint`] takes it.
    drawing: Mutex<Drawing<D>>,
    /// The frame painted last, and new settings for the next paint.
    ready: Mutex<Ready<D::Style>>,
}

struct Ready<T> {
    frame: Arc<VideoFrame>,
    /// Whether the last paint found no new MIDI and nothing moving.
    settled: bool,
    restyle: Option<T>,
}

impl<D: Draw> Painter for MidiPainter<D> {
    fn paint(&self, at: MediaTime) {
        let restyle = lock(&self.ready).restyle.take();
        let mut drawing = lock(&self.drawing);
        if let Some(style) = restyle {
            drawing.draw.restyle(style);
            drawing.drawn = None;
        }
        let settled = drawing.paint(at);
        let frame = drawing.frames.current();
        drop(drawing);
        let mut ready = lock(&self.ready);
        ready.frame = frame;
        ready.settled = settled && ready.restyle.is_none();
    }

    fn frame(&self) -> Arc<VideoFrame> {
        Arc::clone(&lock(&self.ready).frame)
    }

    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        if !self.fed {
            return None;
        }
        if lock(&self.ready).settled {
            return Some(at_millis(millis(at).saturating_add(STILL_MS)));
        }
        Some(next_tick(at))
    }
}

/// Locks `mutex`; a panic while it was held leaves nothing half-done that matters for
/// drawing, so a poisoned lock is used as it is.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Everything a MIDI source draws with.
struct Drawing<D> {
    draw: D,
    state: Arc<MidiState>,
    /// The clock the input's messages are stamped on, once the host sets it; until
    /// then it draws at the paint's time.
    clock: Arc<OnceLock<Arc<dyn MediaClock>>>,
    frames: Frames,
    /// The frame tick drawn last.
    drawn: Option<u64>,
    /// The input's [`changes`](MidiState::changes) when it was drawn last.
    seen: Option<u64>,
    /// Whether the last frame had something still moving.
    moving: bool,
}

impl<D: Draw> Drawing<D> {
    /// Draws the frame for `at`, unless this tick's is drawn already, or the input has
    /// not moved and nothing drawn is moving. Returns whether it settled.
    fn paint(&mut self, at: MediaTime) -> bool {
        let now = tick(at);
        if self.drawn == Some(now) {
            return false;
        }
        let activity = self.state.changes();
        if self.drawn.is_some() && !self.moving && self.seen == Some(activity) {
            return true;
        }
        self.drawn = Some(now);
        self.seen = Some(activity);
        let pts = tick_start(now);
        // A note's age is measured on the clock it was stamped on; the frame's own
        // time runs from the source's start, so it serves only without one.
        let now_ms = self
            .clock
            .get()
            .map_or_else(|| millis(pts), |clock| clock_ms(clock.as_ref()));
        let (draw, state) = (&mut self.draw, &self.state);
        let mut moving = false;
        self.frames.draw(pts, |canvas| {
            moving = draw.draw(state, now_ms, canvas);
        });
        self.moving = moving;
        false
    }
}

/// `clock`'s time now, in whole milliseconds; 0 before its zero.
fn clock_ms(clock: &dyn MediaClock) -> u64 {
    clock
        .now()
        .rescale(Timebase::MILLIS, jstream_media::Rounding::Down)
        .map_or(0, |t| u64::try_from(t.ticks()).unwrap_or(0))
}

/// The frame tick `at` falls in, at [`FPS`].
fn tick(at: MediaTime) -> u64 {
    millis(at).saturating_mul(FPS) / 1000
}

/// When frame tick `tick` starts.
fn tick_start(tick: u64) -> MediaTime {
    at_millis(tick.saturating_mul(1000).div_ceil(FPS))
}

/// The next frame tick's start after `at`.
fn next_tick(at: MediaTime) -> MediaTime {
    tick_start(tick(at).saturating_add(1))
}

/// A frame being drawn: straight sRGB RGBA, rows top first.
pub(crate) struct Canvas<'f> {
    frame: &'f mut VideoFrame,
    width: u32,
    height: u32,
}

impl Canvas<'_> {
    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    /// Fills the whole frame with `color`.
    pub(crate) fn clear(&mut self, color: [u8; 4]) {
        self.fill(0, 0, i64::from(self.width), i64::from(self.height), color);
    }

    /// Fills columns `x0..x1` of rows `y0..y1` with `color`, clipped to the frame.
    pub(crate) fn fill(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, color: [u8; 4]) {
        let clip = |v: i64, max: u32| v.clamp(0, i64::from(max)) as usize;
        let (x0, x1) = (clip(x0, self.width), clip(x1, self.width));
        let (y0, y1) = (clip(y0, self.height), clip(y1, self.height));
        if x0 >= x1 {
            return;
        }
        for y in y0..y1 {
            let Some(row) = self.frame.row_mut(0, y as u32) else {
                break;
            };
            for pixel in row[x0 * 4..x1 * 4].chunks_exact_mut(4) {
                pixel.copy_from_slice(&color);
            }
        }
    }
}

/// The frames a MIDI source draws into. A frame handed out stays valid, so a new one is
/// drawn into a frame no one else holds any more, and only when all are held is another
/// made.
struct Frames {
    size: FrameSize,
    pool: Vec<Arc<VideoFrame>>,
    last: usize,
}

/// Frames kept for reuse, as a scope source keeps them.
const POOL: usize = 4;

impl Frames {
    fn new(size: FrameSize) -> Result<Self, SourceError> {
        Ok(Self {
            size,
            pool: vec![Arc::new(blank(size)?)],
            last: 0,
        })
    }

    fn current(&self) -> Arc<VideoFrame> {
        Arc::clone(&self.pool[self.last])
    }

    /// Draws a frame with `paint`, into a frame no one holds.
    fn draw(&mut self, pts: MediaTime, paint: impl FnOnce(&mut Canvas<'_>)) {
        let free = (0..self.pool.len())
            .filter(|&i| i != self.last)
            .find(|&i| Arc::get_mut(&mut self.pool[i]).is_some());
        let index = match free {
            Some(i) => i,
            None => {
                let Ok(frame) = blank(self.size) else {
                    // Out of memory for another frame: keep showing the last one.
                    return;
                };
                if self.pool.len() < POOL {
                    self.pool.push(Arc::new(frame));
                    self.pool.len() - 1
                } else {
                    let oldest = (self.last + 1) % self.pool.len();
                    self.pool[oldest] = Arc::new(frame);
                    oldest
                }
            }
        };
        let (width, height) = (self.size.width(), self.size.height());
        if let Some(frame) = Arc::get_mut(&mut self.pool[index]) {
            frame.set_pts(pts);
            paint(&mut Canvas {
                frame,
                width,
                height,
            });
            self.last = index;
        }
    }
}

/// A transparent frame of `size`.
fn blank(size: FrameSize) -> Result<VideoFrame, SourceError> {
    let len =
        usize::try_from(u64::from(size.width()) * u64::from(size.height()) * 4).map_err(|_| {
            jstream_media::MediaError::Overflow {
                what: "a MIDI frame",
            }
        })?;
    Ok(rgba_frame(
        size,
        vec![0; len],
        MediaTime::zero(Timebase::MILLIS),
    )?)
}

/// The `width` and `height` settings, within `limits`.
fn frame_size(settings: &Settings, limits: &Limits) -> Result<FrameSize, SourceError> {
    let side = |id: &str| {
        setting(settings, id)?
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .filter(|&v| (1..=limits.max_side).contains(&v))
            .ok_or_else(|| {
                bad(
                    id,
                    &format!("must be a whole number from 1 to {}", limits.max_side),
                )
            })
    };
    let (width, height) = (side("width")?, side("height")?);
    let bytes = u64::from(width) * u64::from(height) * 4 * POOL as u64;
    if bytes > limits.max_decoded_bytes {
        return Err(bad(
            if width >= height { "width" } else { "height" },
            &format!(
                "{POOL} frames of {width}x{height} are over {} bytes",
                limits.max_decoded_bytes
            ),
        ));
    }
    Ok(FrameSize::new(width, height)?)
}

fn bad(param: &str, reason: &str) -> SourceError {
    SourceError::Setting {
        param: param.to_owned(),
        reason: reason.to_owned(),
    }
}

/// The `input` setting: an input's name, or empty for every input.
fn input(settings: &Settings) -> Result<&str, SourceError> {
    let name = setting(settings, "input")?
        .as_str()
        .ok_or_else(|| bad("input", "must be a string"))?;
    if name.len() > MAX_INPUT_NAME || name.chars().any(char::is_control) {
        return Err(bad(
            "input",
            &format!("must be at most {MAX_INPUT_NAME} bytes, with no control characters"),
        ));
    }
    Ok(name)
}

/// A colour setting as straight sRGB RGBA bytes.
fn color(settings: &Settings, id: &str) -> Result<[u8; 4], SourceError> {
    setting(settings, id)?
        .as_str()
        .and_then(parse_color)
        .ok_or_else(|| bad(id, "must be a color as `#rrggbb` or `#rrggbbaa`"))
}

/// A whole-number setting from `min` to `max`.
fn int(settings: &Settings, id: &str, min: u64, max: u64) -> Result<u64, SourceError> {
    setting(settings, id)?
        .as_u64()
        .filter(|v| (min..=max).contains(v))
        .ok_or_else(|| bad(id, &format!("must be a whole number from {min} to {max}")))
}

/// The `channel` setting: 1 to 16, or 0 for any channel, as the channel's wire value.
fn channel(settings: &Settings) -> Result<Option<u8>, SourceError> {
    let channel = int(settings, "channel", 0, 16)?;
    Ok((channel > 0).then(|| (channel - 1) as u8))
}

/// The channels a `channel` setting draws.
fn channels(channel: Option<u8>) -> std::ops::RangeInclusive<u8> {
    channel.map_or(0..=15, |c| c..=c)
}

/// `color` at a brightness for `velocity`: a soft note shows at 35%, a full one at
/// 100%, so every held note is visible.
fn by_velocity(color: [u8; 4], velocity: u8) -> [u8; 4] {
    let scale = 0.35 + 0.65 * f32::from(velocity.min(127)) / 127.0;
    let channel = |c: u8| (f32::from(c) * scale).round() as u8;
    [
        channel(color[0]),
        channel(color[1]),
        channel(color[2]),
        color[3],
    ]
}

#[cfg(test)]
mod tests;
