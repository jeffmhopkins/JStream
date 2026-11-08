//! The JTunnel receiver (ADR-0122, ADR-0123): the engine's one socket for JTunnel
//! senders, a DAW plugin's audio and MIDI over the LAN, and the thread that plays their
//! frames out to the `network_audio` sources' mixer inputs.
//!
//! It is off unless the host turns it on with the senders it accepts
//! ([`JtunnelSetup`]): each is a tunnel name and the address it sends from, both chosen
//! by the person (ADR-0123 §3). The thread subscribes to each accepted sender by name
//! every half second, as JTunnel's keepalive asks, and says bye when it stops. It
//! drops, and counts, every datagram from outside the LAN ranges, from an address it
//! did not subscribe to, or that isn't a JTunnel packet it accepts. It never announces
//! itself and never sends audio or MIDI.
//!
//! Each accepted sender has a [`Playout`] (its jitter buffer, sequence tracking and
//! clock estimate, from `jstream-jtunnel`). The thread pushes each frame in as it
//! arrives and pops each one when it is due, handing its samples to the mixer input of
//! the source that records the sender: an [`AudioInputHandler`], as a device's stream
//! would, with the sender's stream position as the device position and the frame's
//! media-clock time, so the input's own drift estimate and resampling (ADR-0014) apply.
//! A source opens only once its sender has been heard, since the stream's rate is only
//! known then; until then it is unavailable and tried again on every tick, like an
//! unplugged device. A stream that changes rate fails the input, which reopens at the
//! new rate. 24-bit PCM and MIDI-only streams play; Opus is refused and counted until a
//! decoder is picked (ADR-0122 §6).

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver as Inbox, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use jstream_jtunnel::Playout;
use jstream_jtunnel::wire::{self, Codec, Packet};
use jstream_media::{AudioFormat, ChannelLayout, MediaTime, Rounding, SampleFormat, Timebase};
use jstream_platform::{
    AudioInputHandler, AudioPacket, AudioPacketFlags, AudioStream, MediaClock, PlatformError,
};
use jstream_sources::{MidiFeed, MidiInputs};

/// How often a sender is subscribed to again: JTunnel's keepalive.
const SUBSCRIBE_EVERY: Duration = Duration::from_millis(500);
/// The longest the thread waits for a datagram before it checks what is due.
const MAX_WAIT: Duration = Duration::from_millis(5);
/// The shortest wait: a socket timeout of zero would block for ever.
const MIN_WAIT: Duration = Duration::from_micros(500);
/// The most senders one machine accepts.
pub const MAX_SENDERS: usize = 32;
/// Opens and closes waiting for the thread.
const COMMANDS: usize = 64;

/// Whether `ip` is an address JTunnel may be received from or sent to (ADR-0123 §2):
/// loopback, the private IPv4 ranges (10/8, 172.16/12, 192.168/16) and link-local
/// (169.254/16). An IPv4 address mapped into IPv6 counts as itself.
#[must_use]
pub fn is_lan(ip: IpAddr) -> bool {
    let v4 = match ip {
        IpAddr::V4(v4) => v4,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4,
            None => return v6.is_loopback(),
        },
    };
    v4.is_loopback() || v4.is_private() || v4.is_link_local()
}

/// A sender the person accepted on this machine: its tunnel name, and the address and
/// port it sends from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedSender {
    name: String,
    addr: SocketAddr,
}

impl AcceptedSender {
    /// A sender called `name` at `addr`.
    ///
    /// # Errors
    /// A name that is empty, longer than JTunnel's 31 bytes or holds a control
    /// character, or an address outside the LAN ranges ([`is_lan`]) or with port 0.
    pub fn new(name: &str, addr: SocketAddr) -> Result<Self, String> {
        if name.is_empty()
            || name.len() > wire::MAX_TUNNEL_NAME
            || name.chars().any(char::is_control)
        {
            return Err(format!(
                "a JTunnel sender's name must be 1 to {} bytes with no control characters",
                wire::MAX_TUNNEL_NAME
            ));
        }
        if !is_lan(addr.ip()) || addr.port() == 0 {
            return Err(format!(
                "{addr} is not a LAN address and port: JTunnel is received only from \
                 loopback, private and link-local addresses"
            ));
        }
        Ok(Self {
            name: name.to_owned(),
            addr,
        })
    }

    /// The tunnel name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The address it sends from.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl std::str::FromStr for AcceptedSender {
    type Err = String;

    /// Parses `name@address:port`, as the daemon's `--jtunnel-accept` takes it.
    fn from_str(text: &str) -> Result<Self, String> {
        let (name, addr) = text
            .rsplit_once('@')
            .ok_or_else(|| format!("`{text}` is not `name@address:port`"))?;
        let addr = addr
            .parse::<SocketAddr>()
            .map_err(|err| format!("`{addr}` is not an address and port: {err}"))?;
        Self::new(name, addr)
    }
}

/// What the host turns the receiver on with: the senders it accepts, and the local
/// address to receive on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JtunnelSetup {
    /// The accepted senders; a name appears once.
    pub accepted: Vec<AcceptedSender>,
    /// The address the receive socket binds: any IPv4 interface on a port the system
    /// picks, unless the host names one.
    pub bind: SocketAddr,
}

impl JtunnelSetup {
    /// Receives from `accepted` on any interface, on a port the system picks.
    ///
    /// # Errors
    /// More than [`MAX_SENDERS`], or one name twice.
    pub fn new(accepted: Vec<AcceptedSender>) -> Result<Self, String> {
        if accepted.len() > MAX_SENDERS {
            return Err(format!(
                "at most {MAX_SENDERS} JTunnel senders are accepted"
            ));
        }
        for (n, sender) in accepted.iter().enumerate() {
            if accepted[..n].iter().any(|s| s.name == sender.name) {
                return Err(format!(
                    "the JTunnel sender `{}` is named twice",
                    sender.name
                ));
            }
        }
        Ok(Self {
            accepted,
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        })
    }
}

/// What the receiver has dropped and why, and what it played, since it started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JtunnelStats {
    /// Datagrams from outside the LAN ranges.
    pub outside_lan: u64,
    /// Datagrams from a LAN address it didn't subscribe to.
    pub unaccepted: u64,
    /// Datagrams that aren't a JTunnel packet it accepts.
    pub malformed: u64,
    /// Audio packets in a codec it can't decode yet (Opus).
    pub unsupported: u64,
    /// Frames handed to a source's input.
    pub played: u64,
    /// Of those, frames concealed with silence.
    pub concealed: u64,
    /// MIDI messages handed to the MIDI inputs.
    pub midi: u64,
}

/// The receiver's counters, shared with its thread.
#[derive(Debug, Default)]
struct JtunnelCounters {
    outside_lan: AtomicU64,
    unaccepted: AtomicU64,
    malformed: AtomicU64,
    unsupported: AtomicU64,
    played: AtomicU64,
    concealed: AtomicU64,
    midi: AtomicU64,
}

/// One accepted sender, as both sides see it.
struct Sender {
    accepted: AcceptedSender,
    /// Its stream's sample rate, from its latest audio packet; 0 before one.
    rate: AtomicU32,
}

/// What the receiver's thread is asked to do.
enum Command {
    Open {
        sender: usize,
        id: u64,
        rate: u32,
        lead_ns: u64,
        handler: Box<dyn AudioInputHandler>,
    },
    Close {
        sender: usize,
        id: u64,
    },
}

/// The running receiver. Dropping it stops the thread, which says bye to every sender.
pub(crate) struct Receiver {
    senders: Arc<[Sender]>,
    counters: Arc<JtunnelCounters>,
    commands: SyncSender<Command>,
    next_id: AtomicU64,
    local: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for Receiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Receiver")
            .field("local", &self.local)
            .field("senders", &self.senders.len())
            .finish_non_exhaustive()
    }
}

impl Receiver {
    /// Binds the receive socket and starts the thread, stamping arrivals with `clock`.
    ///
    /// # Errors
    /// The socket could not be bound or set up, or the thread could not start.
    pub(crate) fn start(
        setup: &JtunnelSetup,
        clock: Arc<dyn MediaClock>,
        midi: Option<Arc<MidiInputs>>,
    ) -> io::Result<Self> {
        let socket = UdpSocket::bind(setup.bind)?;
        socket.set_read_timeout(Some(MAX_WAIT))?;
        let local = socket.local_addr()?;
        let senders: Arc<[Sender]> = setup
            .accepted
            .iter()
            .map(|accepted| Sender {
                accepted: accepted.clone(),
                rate: AtomicU32::new(0),
            })
            .collect();
        let counters = Arc::new(JtunnelCounters::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (commands, inbox) = sync_channel(COMMANDS);
        let thread = {
            let mut run = Run {
                socket,
                senders: Arc::clone(&senders),
                counters: Arc::clone(&counters),
                clock,
                inbox,
                stop: Arc::clone(&stop),
                streams: senders
                    .iter()
                    .map(|sender| Stream::new(midi.as_deref(), &sender.accepted.name))
                    .collect(),
            };
            thread::Builder::new()
                .name("jstream-jtunnel".into())
                .spawn(move || run.run())?
        };
        Ok(Self {
            senders,
            counters,
            commands,
            next_id: AtomicU64::new(1),
            local,
            stop,
            thread: Mutex::new(Some(thread)),
        })
    }

    /// The address it receives on.
    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// What its counters read now.
    pub(crate) fn stats(&self) -> JtunnelStats {
        let c = &*self.counters;
        let read = |n: &AtomicU64| n.load(Ordering::Relaxed);
        JtunnelStats {
            outside_lan: read(&c.outside_lan),
            unaccepted: read(&c.unaccepted),
            malformed: read(&c.malformed),
            unsupported: read(&c.unsupported),
            played: read(&c.played),
            concealed: read(&c.concealed),
            midi: read(&c.midi),
        }
    }

    fn sender(&self, name: &str) -> Result<usize, String> {
        if name.is_empty() {
            return Err("no JTunnel sender is named".into());
        }
        self.senders
            .iter()
            .position(|s| s.accepted.name == name)
            .ok_or_else(|| format!("the JTunnel sender `{name}` is not accepted on this machine"))
    }

    /// The format the sender `name`'s audio arrives in: stereo at its stream's rate.
    ///
    /// # Errors
    /// A sender not accepted, or not heard from yet.
    pub(crate) fn input_format(&self, name: &str) -> Result<AudioFormat, String> {
        let sender = &self.senders[self.sender(name)?];
        let rate = sender.rate.load(Ordering::Acquire);
        if rate == 0 {
            return Err(format!(
                "nothing has arrived from the JTunnel sender `{name}` at {} yet",
                sender.accepted.addr
            ));
        }
        AudioFormat::new(rate, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .map_err(|err| err.to_string())
    }

    /// Starts handing the sender `name`'s frames to `handler`, each held `latency_ms`
    /// past its jitter-free arrival, until the returned stream is dropped. Opening the
    /// same sender again replaces the earlier handler.
    ///
    /// # Errors
    /// As for [`input_format`](Self::input_format), or the thread has stopped.
    pub(crate) fn open(
        &self,
        name: &str,
        latency_ms: u32,
        handler: Box<dyn AudioInputHandler>,
    ) -> Result<Box<dyn AudioStream>, String> {
        let format = self.input_format(name)?;
        let sender = self.sender(name)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.commands
            .try_send(Command::Open {
                sender,
                id,
                rate: format.sample_rate(),
                lead_ns: u64::from(latency_ms) * 1_000_000,
                handler,
            })
            .map_err(|_| "the JTunnel receiver is not taking inputs".to_owned())?;
        Ok(Box::new(NetworkStream {
            format,
            sender,
            id,
            commands: self.commands.clone(),
        }))
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let thread = self
            .thread
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

/// A sender's audio, flowing into one handler until dropped.
struct NetworkStream {
    format: AudioFormat,
    sender: usize,
    id: u64,
    commands: SyncSender<Command>,
}

impl AudioStream for NetworkStream {
    fn format(&self) -> AudioFormat {
        self.format
    }
}

impl Drop for NetworkStream {
    fn drop(&mut self) {
        // A full queue or a stopped thread: the handler goes when the thread does, or
        // when the next open for this sender replaces it.
        let _ = self.commands.try_send(Command::Close {
            sender: self.sender,
            id: self.id,
        });
    }
}

/// One sender's stream on the thread.
struct Stream {
    playout: Playout,
    handler: Option<Handler>,
    /// Where its MIDI goes: its own input's state and the every-input one, looked up
    /// once so the thread takes no lock per message, and its clock's tempo (ADR-0142).
    midi: Option<MidiFeed>,
}

struct Handler {
    id: u64,
    rate: u32,
    handler: Box<dyn AudioInputHandler>,
}

impl Stream {
    fn new(midi: Option<&MidiInputs>, name: &str) -> Self {
        Self {
            playout: Playout::new(u64::from(jstream_sources::LATENCY_MS.2) * 1_000_000),
            handler: None,
            midi: midi.map(|inputs| inputs.feed(name)),
        }
    }
}

/// The thread's state.
struct Run {
    socket: UdpSocket,
    senders: Arc<[Sender]>,
    counters: Arc<JtunnelCounters>,
    clock: Arc<dyn MediaClock>,
    inbox: Inbox<Command>,
    stop: Arc<AtomicBool>,
    streams: Vec<Stream>,
}

impl Run {
    fn run(&mut self) {
        let mut datagram = [0_u8; wire::MAX_PACKET_BYTES + 1];
        let mut samples = [0.0_f32; wire::MAX_FRAME_SAMPLES as usize * wire::CHANNELS as usize];
        let mut next_subscribe = Instant::now();
        while !self.stop.load(Ordering::Acquire) {
            if Instant::now() >= next_subscribe {
                self.send_all(wire::write_subscribe);
                next_subscribe += SUBSCRIBE_EVERY;
                // After a stall, the next keepalive goes out now and then on time.
                next_subscribe = next_subscribe.max(Instant::now());
            }
            self.commands();
            let _ = self.socket.set_read_timeout(Some(self.wait()));
            // An error is a timeout, or an ICMP port-unreachable from a sender not
            // running yet, reported on the next receive on some systems.
            if let Ok((len, from)) = self.socket.recv_from(&mut datagram) {
                self.receive(&datagram[..len], from, &mut samples);
            }
            self.play();
        }
        self.send_all(|out, _| wire::write_bye(out));
    }

    /// How long to wait for a datagram: until the next frame is due, within bounds.
    fn wait(&self) -> Duration {
        let now = self.now_ns();
        self.streams
            .iter()
            .filter(|s| s.handler.is_some())
            .filter_map(|s| s.playout.next_due_ns())
            .min()
            .map_or(MAX_WAIT, |due| {
                Duration::from_nanos(due.saturating_sub(now))
            })
            .clamp(MIN_WAIT, MAX_WAIT)
    }

    fn send_all(&self, write: impl Fn(&mut [u8], &[u8]) -> Option<usize>) {
        let mut out = [0_u8; 64];
        for sender in self.senders.iter() {
            if let Some(len) = write(&mut out, sender.accepted.name.as_bytes()) {
                // ADR-0123 §2: only to LAN addresses, which `AcceptedSender` ensures.
                let _ = self.socket.send_to(&out[..len], sender.accepted.addr);
            }
        }
    }

    fn commands(&mut self) {
        loop {
            match self.inbox.try_recv() {
                Ok(Command::Open {
                    sender,
                    id,
                    rate,
                    lead_ns,
                    handler,
                }) => {
                    if let Some(stream) = self.streams.get_mut(sender) {
                        stream.playout.set_lead_ns(lead_ns);
                        stream.handler = Some(Handler { id, rate, handler });
                    }
                }
                Ok(Command::Close { sender, id }) => {
                    if let Some(stream) = self.streams.get_mut(sender)
                        && stream.handler.as_ref().is_some_and(|h| h.id == id)
                    {
                        stream.handler = None;
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }

    fn now_ns(&self) -> u64 {
        self.clock
            .now()
            .rescale(Timebase::NANOS, Rounding::Down)
            .map_or(0, |t| u64::try_from(t.ticks()).unwrap_or(0))
    }

    fn receive(&mut self, datagram: &[u8], from: SocketAddr, samples: &mut [f32]) {
        let counters = &self.counters;
        if !is_lan(from.ip()) {
            counters.outside_lan.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let Some(index) = self
            .senders
            .iter()
            .position(|s| same(s.accepted.addr, from))
        else {
            counters.unaccepted.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let Ok(Packet::Audio { header, frames }) = wire::parse(datagram) else {
            // A subscribe or bye is a receiver's, never a sender's.
            counters.malformed.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if header.codec == Codec::Opus {
            counters.unsupported.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.senders[index]
            .rate
            .store(header.sample_rate, Ordering::Release);
        let arrival = self.now_ns();
        let stream = &mut self.streams[index];
        // Oldest first: the redundant copy of the frame before, then the newest.
        for frame in frames.iter().rev().flatten() {
            let decoded = match header.codec {
                Codec::Pcm24 => wire::decode_pcm24(frame.audio, samples),
                Codec::None | Codec::Opus => 0,
            };
            stream
                .playout
                .push(&header, frame, &samples[..decoded], arrival);
        }
    }

    fn play(&mut self) {
        let now = self.now_ns();
        for (index, stream) in self.streams.iter_mut().enumerate() {
            let rate = self.senders[index].rate.load(Ordering::Acquire);
            if let Some(handler) = &stream.handler
                && handler.rate != rate
            {
                // The stream changed rate: the input reopens at the new one.
                let mut gone = stream.handler.take();
                if let Some(h) = gone.as_mut() {
                    h.handler.on_error(&PlatformError::DeviceLost {
                        what: format!("the JTunnel sender `{}`", self.senders[index].accepted.name),
                        reason: format!("its stream changed rate to {rate} Hz"),
                    });
                }
                drop(gone);
            }
            while let Some(due) = stream.playout.pop(now) {
                // MIDI is drawn whether or not a source records the audio, stamped at
                // its sample within the frame.
                if let Some(feed) = &mut stream.midi {
                    for (offset, event) in due.events() {
                        if let wire::Event::Midi(message) = event {
                            let ns = due.time_ns.saturating_add(
                                u64::from(offset) * 1_000_000_000 / u64::from(rate.max(1)),
                            );
                            feed.push(ns, &message);
                            self.counters.midi.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                let Some(handler) = stream.handler.as_mut() else {
                    continue;
                };
                // A MIDI-only stream's frames carry no audio for the mixer.
                if due.samples.is_empty() {
                    continue;
                }
                self.counters.played.fetch_add(1, Ordering::Relaxed);
                if due.concealed {
                    self.counters.concealed.fetch_add(1, Ordering::Relaxed);
                }
                let time = i64::try_from(due.time_ns)
                    .ok()
                    .map(|ns| MediaTime::new(ns, Timebase::NANOS));
                handler.handler.on_packet(&AudioPacket {
                    samples: due.samples,
                    device_position: due.position,
                    time,
                    flags: AudioPacketFlags {
                        discontinuity: due.discontinuity,
                        silent: due.concealed,
                    },
                });
            }
        }
    }
}

/// Whether a datagram from `from` is from the sender at `addr`: the same IPv4 address
/// whether or not it came mapped into IPv6, and the same port.
fn same(addr: SocketAddr, from: SocketAddr) -> bool {
    let plain = |ip: IpAddr| match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    };
    addr.port() == from.port() && plain(addr.ip()) == plain(from.ip())
}

#[cfg(test)]
mod tests;
