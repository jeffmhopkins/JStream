//! Reconnect with backoff: a publish session that survives a dropped connection.
//!
//! [`ReconnectingPublisher`] wraps an [`RtmpPublisher`]. When a send or a poll fails
//! because the link is gone, it drops the connection and retries with exponential
//! backoff and jitter on a short-lived background thread, so the caller's thread never
//! waits for a reconnect. Packets sent meanwhile are dropped and counted, never queued
//! (docs/RULES.md#R-RT-004). Once a new session is published it re-sends the cached
//! `onMetaData` and sequence headers and resumes video at the next keyframe.
//!
//! Docs: crates/jstream-outputs/README.md#purpose

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{RtmpError, RtmpPublisher, RtmpUrl, StreamKey, Transport, connect};

/// When to retry a lost connection and when to give up.
///
/// The delay before reconnect attempt *n* (counting from 0) is
/// `min(initial_delay × 2ⁿ, max_delay)`, scaled by a random factor in `[0.5, 1]`
/// ("equal jitter"), so outputs that lost the same ingest don't retry in lockstep while
/// each wait keeps a floor. The publisher gives up when `max_attempts` attempts have
/// been made, or when the next attempt would start more than `max_outage` after the
/// connection was lost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// The delay before the first attempt. Default 1 s.
    pub initial_delay: Duration,
    /// The longest delay between attempts. Default 30 s.
    pub max_delay: Duration,
    /// Give up after this many attempts in one outage; `None` for no limit. `Some(0)`
    /// never reconnects. Default `None`: the outage limit decides.
    pub max_attempts: Option<u32>,
    /// Give up once an outage would last longer than this; `None` for no limit.
    /// Default 5 minutes.
    pub max_outage: Option<Duration>,
    /// A reconnected session that drops again sooner than this continues the previous
    /// outage (its attempt count, backoff step and start time) instead of starting a
    /// new one, so a server that accepts and then drops at once can't keep the
    /// publisher retrying forever at the shortest delay. Default 30 s.
    pub stable_after: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            max_attempts: None,
            max_outage: Some(Duration::from_secs(5 * 60)),
            stable_after: Duration::from_secs(30),
        }
    }
}

/// The time source for backoff. [`Clock::system`] reads [`Instant::now`]; tests inject
/// a clock they advance by hand.
#[derive(Clone)]
pub struct Clock(Arc<dyn Fn() -> Instant + Send + Sync>);

impl Clock {
    /// The monotonic system clock.
    pub fn system() -> Self {
        Self(Arc::new(Instant::now))
    }

    /// A clock that reads `now`.
    pub fn from_fn(now: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        Self(Arc::new(now))
    }

    /// The current time.
    pub fn now(&self) -> Instant {
        (self.0)()
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::system()
    }
}

impl std::fmt::Debug for Clock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clock")
    }
}

/// Opens and publishes a new session, the same way the first one was. It runs on a
/// background thread and should bound its own blocking (connect and read timeouts), as
/// [`connect_publisher`] does.
pub type Connector<S> = Arc<dyn Fn() -> Result<RtmpPublisher<S>, RtmpError> + Send + Sync>;

/// Timeouts for [`connect_publisher`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionTimeouts {
    /// Bounds the TCP connection, the TLS handshake and each read while the RTMP
    /// handshake and publish commands run. Default 10 s.
    pub connect: Duration,
    /// Bounds each write for the whole session, so a stalled server surfaces as an
    /// error. Default 10 s.
    pub write: Duration,
    /// The read timeout once publishing, which is how long
    /// [`RtmpPublisher::poll_incoming`] may wait when nothing is pending. Default 1 ms.
    pub poll: Duration,
}

impl Default for SessionTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            write: Duration::from_secs(10),
            poll: Duration::from_millis(1),
        }
    }
}

/// Connects to `url` and publishes `key`, with `timeouts` applied: the publish
/// sequence reads with the `connect` timeout, and only once publishing has started is
/// the read timeout shortened to `poll`. This opens a network connection.
///
/// # Errors
/// Those of [`connect`] and [`RtmpPublisher::publish`], and [`RtmpError::Io`] when the
/// operating system refuses a timeout (for example a zero duration).
pub fn connect_publisher(
    url: &RtmpUrl,
    key: &StreamKey,
    timeouts: &SessionTimeouts,
) -> Result<RtmpPublisher<Transport>, RtmpError> {
    let io = |e| RtmpError::io("setting socket timeouts", e);
    let transport = connect(url, Some(timeouts.connect))?;
    transport
        .set_read_timeout(Some(timeouts.connect))
        .map_err(io)?;
    transport
        .set_write_timeout(Some(timeouts.write))
        .map_err(io)?;
    let publisher = RtmpPublisher::publish(transport, url, key)?;
    publisher
        .get_ref()
        .set_read_timeout(Some(timeouts.poll))
        .map_err(io)?;
    Ok(publisher)
}

/// A [`Connector`] that runs [`connect_publisher`] for `url` and `key`.
pub fn rtmp_connector(
    url: RtmpUrl,
    key: StreamKey,
    timeouts: SessionTimeouts,
) -> Connector<Transport> {
    Arc::new(move || connect_publisher(&url, &key, &timeouts))
}

/// Whether the publish link is up, being re-established, or abandoned. A host maps
/// these onto its output states (`active`, `reconnecting`, `failed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkState {
    /// Publishing: packets reach the server (after a reconnect, video from the next
    /// keyframe on).
    Active,
    /// The connection was lost; packets are dropped while attempts run.
    Reconnecting,
    /// The policy's limit was reached. Every later call fails with
    /// [`RtmpError::ReconnectGaveUp`].
    Failed,
}

/// Running totals since the publisher was created.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReconnectStats {
    /// Sessions re-established.
    pub reconnects: u64,
    /// Reconnect attempts started.
    pub attempts: u64,
    /// Attempts that failed, including sessions that dropped again before
    /// `stable_after`.
    pub failed_attempts: u64,
    /// Video packets not delivered: the one whose send failed, those sent while
    /// reconnecting, and those before the first keyframe after a reconnect. Sequence
    /// headers are not counted; they are cached and re-sent.
    pub dropped_video: u64,
    /// Audio packets not delivered, counted as for video. After a reconnect, audio
    /// waits for the video keyframe when the stream has video.
    pub dropped_audio: u64,
}

impl ReconnectStats {
    /// All dropped media packets.
    pub fn dropped(&self) -> u64 {
        self.dropped_video.saturating_add(self.dropped_audio)
    }
}

/// What a resumed session still has to send before media flows normally.
#[derive(Clone, Copy, Debug, Default)]
struct Resync {
    /// `onMetaData` goes out before the first resumed packet.
    metadata: bool,
    /// Video waits for a keyframe, and audio waits with it.
    video: bool,
    /// The AAC sequence header goes out before the next audio packet.
    audio_header: bool,
}

/// One outage: the connection was lost at `since`.
struct Outage<S> {
    since: Instant,
    attempts: u32,
    failures: u32,
    next_at: Instant,
    pending: Option<Receiver<Result<RtmpPublisher<S>, RtmpError>>>,
}

enum Link<S> {
    Up {
        /// Boxed: the session's buffers would make every state as large as this one.
        publisher: Box<RtmpPublisher<S>>,
        resync: Resync,
        /// For a reconnected session: when it came up, and the outage it ended.
        reconnected: Option<(Instant, Outage<S>)>,
    },
    Down(Outage<S>),
    Failed {
        attempts: u32,
        outage: Duration,
    },
}

/// The last sequence headers and metadata the caller sent.
#[derive(Default)]
struct Headers {
    metadata: Option<Vec<u8>>,
    /// The AVC sequence header, or the enhanced RTMP `SequenceStart` (HEVC, AV1).
    video: Option<Vec<u8>>,
    aac: Option<Vec<u8>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Packet {
    Header,
    Keyframe,
    Other,
}

/// The `IsExHeader` bit of a VIDEODATA body's first byte: an enhanced RTMP tag.
const EX_HEADER: u8 = 0x80;
/// FLV `CodecID` of AVC (H.264).
const CODEC_AVC: u8 = 7;
/// The `FrameType` of a keyframe.
const FRAME_KEY: u8 = 1;
/// Enhanced RTMP `VideoPacketType`s (`jstream_mux::flv::ExVideoPacketType`).
const EX_SEQUENCE_START: u8 = 0;
const EX_CODED_FRAMES: u8 = 1;
const EX_CODED_FRAMES_X: u8 = 3;

/// A VIDEODATA body: a sequence header, a keyframe, or other.
///
/// Legacy tags (AVC): the high nibble is the frame type, the low one the codec, and an
/// `AVCPacketType` of 0 is the sequence header. Enhanced RTMP tags (HEVC, AV1) set
/// `IsExHeader`, then carry a 3-bit frame type and a 4-bit `VideoPacketType`:
/// `SequenceStart` is the header, and a keyframe is `CodedFrames` or `CodedFramesX`
/// with frame type 1. Every other packet type (`SequenceEnd`, `Metadata`,
/// `MPEG2TSSequenceStart`, `Multitrack`, `ModEx`) and command frames are other: they
/// are neither kept for re-sending nor a point to resume at.
fn classify_video(body: &[u8]) -> Packet {
    match body {
        [first, ..] if first & EX_HEADER != 0 => {
            let frame_type = (first >> 4) & 0x07;
            match first & 0x0F {
                EX_SEQUENCE_START => Packet::Header,
                EX_CODED_FRAMES | EX_CODED_FRAMES_X if frame_type == FRAME_KEY => Packet::Keyframe,
                _ => Packet::Other,
            }
        }
        [first, 0, ..] if first & 0x0F == CODEC_AVC => Packet::Header,
        [first, ..] if first >> 4 == FRAME_KEY => Packet::Keyframe,
        _ => Packet::Other,
    }
}

/// An AAC AUDIODATA body: a sequence header (`AACPacketType` 0) or other.
fn classify_audio(body: &[u8]) -> Packet {
    match body {
        [first, 0, ..] if first >> 4 == 10 => Packet::Header,
        _ => Packet::Other,
    }
}

/// Whether `err` means the link is unusable, as opposed to a bad argument from the
/// caller, which leaves the session as it was.
fn link_lost(err: &RtmpError) -> bool {
    !matches!(
        err,
        RtmpError::PayloadSize { .. }
            | RtmpError::Timestamp(_)
            | RtmpError::NegativeTimestamp { .. }
            | RtmpError::InvalidChunkSize { .. }
    )
}

/// Equal-jitter exponential backoff on a small xorshift generator.
struct Backoff {
    state: u64,
}

impl Backoff {
    fn new(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// The wait before attempt `n` (0 for the first): in `[d / 2, d]` with
    /// `d = min(initial × 2ⁿ, max)`.
    fn delay(&mut self, policy: &ReconnectPolicy, n: u32) -> Duration {
        let factor = 2u32.saturating_pow(n);
        let full = policy
            .initial_delay
            .saturating_mul(factor)
            .min(policy.max_delay);
        let floor = full / 2;
        let span = u64::try_from((full - floor).as_nanos()).unwrap_or(u64::MAX);
        let extra = match span.checked_add(1) {
            Some(range) => self.next_u64() % range,
            None => self.next_u64(),
        };
        floor + Duration::from_nanos(extra)
    }
}

/// An RTMP publish session that reconnects when its connection drops.
///
/// Use it from one thread, like [`RtmpPublisher`]. Calls never wait for a reconnect:
/// attempts run on a background thread (one at a time), and every call checks whether
/// one has finished or is due. While the link is down, media is dropped and counted in
/// [`stats`](Self::stats); sequence headers and `onMetaData` are remembered. After a
/// reconnect the publisher sends `onMetaData` and the video sequence header (AVC, or an
/// enhanced RTMP `SequenceStart` for HEVC and AV1) just before the first video keyframe (dropping video and audio until then; an audio-only stream
/// resumes at once) and the AAC sequence header just before the first audio packet,
/// each at that packet's timestamp. Timestamps continue from the old session.
///
/// Read [`state`](Self::state) after each call to report `reconnecting`, `active` and
/// `failed`. Only giving up is an error ([`RtmpError::ReconnectGaveUp`]); a lost
/// connection is not. Errors about the caller's input ([`RtmpError::PayloadSize`],
/// timestamp errors) are returned as before and leave the link alone.
///
/// The stream key is never logged: logs carry only [`RtmpError`] text, which holds no
/// key (docs/RULES.md#R-SEC-001).
pub struct ReconnectingPublisher<S> {
    link: Link<S>,
    connector: Connector<S>,
    policy: ReconnectPolicy,
    clock: Clock,
    backoff: Backoff,
    headers: Headers,
    seen_video: bool,
    stats: ReconnectStats,
}

impl<S: Read + Write + Send + 'static> ReconnectingPublisher<S> {
    /// Wraps the published session `publisher`. `connector` opens each replacement
    /// session; for TCP and TLS use [`rtmp_connector`] with the same URL and key.
    pub fn new(
        publisher: RtmpPublisher<S>,
        connector: Connector<S>,
        policy: ReconnectPolicy,
        clock: Clock,
    ) -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);
        Self {
            link: Link::Up {
                publisher: Box::new(publisher),
                resync: Resync::default(),
                reconnected: None,
            },
            connector,
            policy,
            clock,
            backoff: Backoff::new(seed),
            headers: Headers::default(),
            seen_video: false,
            stats: ReconnectStats::default(),
        }
    }

    /// The link's state after the last call.
    pub fn state(&self) -> LinkState {
        match self.link {
            Link::Up { .. } => LinkState::Active,
            Link::Down(_) => LinkState::Reconnecting,
            Link::Failed { .. } => LinkState::Failed,
        }
    }

    /// Reconnect and drop counts so far.
    pub fn stats(&self) -> ReconnectStats {
        self.stats
    }

    /// The policy in use.
    pub fn policy(&self) -> &ReconnectPolicy {
        &self.policy
    }

    /// The current session, while the link is up.
    pub fn publisher(&self) -> Option<&RtmpPublisher<S>> {
        match &self.link {
            Link::Up { publisher, .. } => Some(&**publisher),
            _ => None,
        }
    }

    /// The current session, mutably, while the link is up: for example to bound its
    /// writes ([`Transport::set_write_deadline`]).
    pub fn publisher_mut(&mut self) -> Option<&mut RtmpPublisher<S>> {
        match &mut self.link {
            Link::Up { publisher, .. } => Some(&mut **publisher),
            _ => None,
        }
    }

    /// Sends `onMetaData` as [`RtmpPublisher::send_metadata`] does, and keeps it to
    /// re-send after a reconnect. While reconnecting it is only kept.
    ///
    /// # Errors
    /// [`RtmpError::ReconnectGaveUp`] once the publisher has given up, and
    /// [`RtmpError::PayloadSize`] for an empty body.
    pub fn send_metadata(&mut self, on_metadata: &[u8]) -> Result<(), RtmpError> {
        if on_metadata.is_empty() {
            return Err(RtmpError::PayloadSize { len: 0 });
        }
        self.headers.metadata = Some(on_metadata.to_vec());
        self.drive()?;
        let result = match &mut self.link {
            Link::Up {
                publisher, resync, ..
            } if !resync.metadata => publisher.send_metadata(on_metadata),
            // Down, or resuming: it goes out with the first resumed packet.
            _ => return Ok(()),
        };
        self.after_send(result, None)
    }

    /// Sends a VIDEODATA tag body as [`RtmpPublisher::send_video`] does. Sequence
    /// headers (AVC, or an enhanced RTMP `SequenceStart` for HEVC and AV1) are kept to
    /// re-send after a reconnect.
    ///
    /// # Errors
    /// [`RtmpError::ReconnectGaveUp`] once the publisher has given up, and the caller
    /// errors [`RtmpPublisher::send_video`] reports (an empty or oversized body).
    pub fn send_video(&mut self, timestamp_ms: u32, tag_body: &[u8]) -> Result<(), RtmpError> {
        let kind = classify_video(tag_body);
        if kind == Packet::Header {
            self.headers.video = Some(tag_body.to_vec());
        } else {
            self.seen_video = true;
        }
        self.drive()?;
        let counted = kind != Packet::Header;
        let result = match &mut self.link {
            Link::Up {
                publisher, resync, ..
            } => {
                if resync.video {
                    match kind {
                        // Kept; it goes out with the keyframe.
                        Packet::Header => return Ok(()),
                        Packet::Other => {
                            self.stats.dropped_video += 1;
                            return Ok(());
                        }
                        Packet::Keyframe => {
                            resync.video = false;
                            let metadata = std::mem::take(&mut resync.metadata);
                            resume_video(publisher, &self.headers, metadata, timestamp_ms)
                                .and_then(|()| publisher.send_video(timestamp_ms, tag_body))
                        }
                    }
                } else {
                    publisher.send_video(timestamp_ms, tag_body)
                }
            }
            _ => {
                if counted {
                    self.stats.dropped_video += 1;
                }
                return Ok(());
            }
        };
        self.after_send(result, counted.then_some(MediaKind::Video))
    }

    /// Sends an AUDIODATA tag body as [`RtmpPublisher::send_audio`] does. AAC sequence
    /// headers are kept to re-send after a reconnect.
    ///
    /// # Errors
    /// [`RtmpError::ReconnectGaveUp`] once the publisher has given up, and the caller
    /// errors [`RtmpPublisher::send_audio`] reports (an empty or oversized body).
    pub fn send_audio(&mut self, timestamp_ms: u32, tag_body: &[u8]) -> Result<(), RtmpError> {
        let kind = classify_audio(tag_body);
        if kind == Packet::Header {
            self.headers.aac = Some(tag_body.to_vec());
        }
        self.drive()?;
        let counted = kind != Packet::Header;
        let result = match &mut self.link {
            Link::Up {
                publisher, resync, ..
            } => {
                if kind == Packet::Header && resync.audio_header {
                    // Kept; it goes out before the next audio packet.
                    return Ok(());
                }
                if kind != Packet::Header && resync.video {
                    self.stats.dropped_audio += 1;
                    return Ok(());
                }
                let metadata = kind != Packet::Header && std::mem::take(&mut resync.metadata);
                let header = kind != Packet::Header && std::mem::take(&mut resync.audio_header);
                resume_audio(publisher, &self.headers, metadata, header, timestamp_ms)
                    .and_then(|()| publisher.send_audio(timestamp_ms, tag_body))
            }
            _ => {
                if counted {
                    self.stats.dropped_audio += 1;
                }
                return Ok(());
            }
        };
        self.after_send(result, counted.then_some(MediaKind::Audio))
    }

    /// Reads what the server sent, as [`RtmpPublisher::poll_incoming`] does, and moves
    /// a reconnect along: collects a finished attempt, or starts one that is due. Call
    /// it regularly, also while no media flows.
    ///
    /// # Errors
    /// [`RtmpError::ReconnectGaveUp`] once the publisher has given up.
    pub fn poll(&mut self) -> Result<(), RtmpError> {
        self.drive()?;
        let result = match &mut self.link {
            Link::Up { publisher, .. } => publisher.poll_incoming(),
            _ => return Ok(()),
        };
        self.after_send(result, None)
    }

    /// Ends the session: [`RtmpPublisher::finish`] while the link is up, returning the
    /// transport; `None` while reconnecting (an attempt still running is abandoned and
    /// its connection closed) or after giving up.
    ///
    /// # Errors
    /// Transport errors from [`RtmpPublisher::finish`].
    pub fn finish(self) -> Result<Option<S>, RtmpError> {
        match self.link {
            Link::Up { publisher, .. } => (*publisher).finish().map(Some),
            Link::Down(_) | Link::Failed { .. } => Ok(None),
        }
    }

    /// Handles the result of a send on the live session.
    fn after_send(
        &mut self,
        result: Result<(), RtmpError>,
        packet: Option<MediaKind>,
    ) -> Result<(), RtmpError> {
        match result {
            Ok(()) => Ok(()),
            Err(err) if !link_lost(&err) => Err(err),
            Err(err) => {
                match packet {
                    Some(MediaKind::Video) => self.stats.dropped_video += 1,
                    Some(MediaKind::Audio) => self.stats.dropped_audio += 1,
                    None => {}
                }
                self.lose(&err)
            }
        }
    }

    /// The live session failed with `err`: drop it and start (or continue) an outage.
    fn lose(&mut self, err: &RtmpError) -> Result<(), RtmpError> {
        let now = self.clock.now();
        let reconnected = match self.take_link() {
            Link::Up { reconnected, .. } => reconnected,
            other => {
                self.link = other;
                return Ok(());
            }
        };
        let outage = match reconnected {
            Some((up_at, mut outage))
                if now.saturating_duration_since(up_at) < self.policy.stable_after =>
            {
                tracing::warn!(error = %err, "RTMP connection lost again soon after reconnecting");
                self.stats.failed_attempts += 1;
                outage.failures += 1;
                outage.pending = None;
                outage
            }
            _ => {
                tracing::warn!(error = %err, "RTMP connection lost; reconnecting");
                Outage {
                    since: now,
                    attempts: 0,
                    failures: 0,
                    next_at: now,
                    pending: None,
                }
            }
        };
        self.schedule(outage, now, err)
    }

    /// Schedules the next attempt of `outage`, or gives up.
    fn schedule(
        &mut self,
        mut outage: Outage<S>,
        now: Instant,
        last: &RtmpError,
    ) -> Result<(), RtmpError> {
        if self
            .policy
            .max_attempts
            .is_some_and(|max| outage.attempts >= max)
        {
            return self.give_up(&outage, now, last);
        }
        let next_at = now + self.backoff.delay(&self.policy, outage.failures);
        if self
            .policy
            .max_outage
            .is_some_and(|max| next_at.saturating_duration_since(outage.since) > max)
        {
            return self.give_up(&outage, now, last);
        }
        outage.next_at = next_at;
        self.link = Link::Down(outage);
        Ok(())
    }

    fn give_up(
        &mut self,
        outage: &Outage<S>,
        now: Instant,
        last: &RtmpError,
    ) -> Result<(), RtmpError> {
        let attempts = outage.attempts;
        let outage = now.saturating_duration_since(outage.since);
        tracing::error!(attempts, ?outage, error = %last, "gave up reconnecting to the RTMP server");
        self.link = Link::Failed { attempts, outage };
        Err(RtmpError::ReconnectGaveUp { attempts, outage })
    }

    /// Takes the link out, leaving a placeholder the caller replaces.
    fn take_link(&mut self) -> Link<S> {
        std::mem::replace(
            &mut self.link,
            Link::Failed {
                attempts: 0,
                outage: Duration::ZERO,
            },
        )
    }

    /// Moves a reconnect along. Cheap while the link is up.
    fn drive(&mut self) -> Result<(), RtmpError> {
        let mut outage = match self.take_link() {
            Link::Down(outage) => outage,
            other => {
                let result = match &other {
                    Link::Failed { attempts, outage } => Err(RtmpError::ReconnectGaveUp {
                        attempts: *attempts,
                        outage: *outage,
                    }),
                    _ => Ok(()),
                };
                self.link = other;
                return result;
            }
        };
        let now = self.clock.now();
        if let Some(pending) = &outage.pending {
            let failed = match pending.try_recv() {
                Ok(Ok(publisher)) => {
                    self.reconnected(publisher, outage, now);
                    return Ok(());
                }
                Ok(Err(err)) => err,
                Err(TryRecvError::Empty) => {
                    // An attempt that outlives the outage limit is abandoned.
                    if self
                        .policy
                        .max_outage
                        .is_some_and(|max| now.saturating_duration_since(outage.since) > max)
                    {
                        let timeout =
                            RtmpError::io("reconnecting", io::Error::from(io::ErrorKind::TimedOut));
                        return self.give_up(&outage, now, &timeout);
                    }
                    self.link = Link::Down(outage);
                    return Ok(());
                }
                Err(TryRecvError::Disconnected) => RtmpError::io(
                    "reconnecting",
                    io::Error::other("the reconnect attempt ended without a result"),
                ),
            };
            tracing::warn!(attempt = outage.attempts, error = %failed, "RTMP reconnect attempt failed");
            self.stats.failed_attempts += 1;
            outage.failures += 1;
            outage.pending = None;
            return self.schedule(outage, now, &failed);
        }
        if now >= outage.next_at {
            outage.attempts += 1;
            self.stats.attempts += 1;
            let attempt = outage.attempts;
            tracing::info!(attempt, "reconnecting to the RTMP server");
            let (tx, rx) = mpsc::sync_channel(1);
            let connector = Arc::clone(&self.connector);
            let span = tracing::info_span!("rtmp_reconnect", attempt);
            let spawned = thread::Builder::new()
                .name("jstream-rtmp-reconnect".into())
                .spawn(move || {
                    let _entered = span.enter();
                    // The receiver is gone if the publisher finished meanwhile; the
                    // new session is then dropped, which closes it.
                    let _ = tx.send(connector());
                });
            match spawned {
                Ok(_) => outage.pending = Some(rx),
                Err(err) => {
                    // No thread for the attempt: count it as failed and back off.
                    let err = RtmpError::io("starting a reconnect attempt", err);
                    self.stats.failed_attempts += 1;
                    outage.failures += 1;
                    return self.schedule(outage, now, &err);
                }
            }
        }
        self.link = Link::Down(outage);
        Ok(())
    }

    /// An attempt of `outage` published a new session.
    fn reconnected(&mut self, publisher: RtmpPublisher<S>, mut outage: Outage<S>, now: Instant) {
        outage.pending = None;
        self.stats.reconnects += 1;
        tracing::info!(
            attempts = outage.attempts,
            outage = ?now.saturating_duration_since(outage.since),
            "reconnected to the RTMP server"
        );
        self.link = Link::Up {
            publisher: Box::new(publisher),
            resync: Resync {
                metadata: self.headers.metadata.is_some(),
                // A cached video header with no frame yet still needs the resync, or
                // the first keyframe would go out without it (#127 review B1).
                video: self.seen_video || self.headers.video.is_some(),
                audio_header: self.headers.aac.is_some(),
            },
            reconnected: Some((now, outage)),
        };
    }
}

#[derive(Clone, Copy)]
enum MediaKind {
    Video,
    Audio,
}

/// Before the first keyframe after a reconnect: `onMetaData` if `metadata`, then the
/// video sequence header, both at the keyframe's timestamp.
fn resume_video<S: Read + Write>(
    publisher: &mut RtmpPublisher<S>,
    headers: &Headers,
    metadata: bool,
    timestamp_ms: u32,
) -> Result<(), RtmpError> {
    if metadata && let Some(body) = &headers.metadata {
        publisher.send_metadata(body)?;
    }
    if let Some(header) = &headers.video {
        publisher.send_video(timestamp_ms, header)?;
    }
    Ok(())
}

/// Before the first audio packet after a reconnect: `onMetaData` if `metadata` (an
/// audio-only stream), and the AAC sequence header if `header`, at its timestamp.
fn resume_audio<S: Read + Write>(
    publisher: &mut RtmpPublisher<S>,
    headers: &Headers,
    metadata: bool,
    header: bool,
    timestamp_ms: u32,
) -> Result<(), RtmpError> {
    if metadata && let Some(body) = &headers.metadata {
        publisher.send_metadata(body)?;
    }
    if header && let Some(aac) = &headers.aac {
        publisher.send_audio(timestamp_ms, aac)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
