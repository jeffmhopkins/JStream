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
//! **Finding senders** (ADR-0123 §3). Every 10 s the thread asks the LAN for JTunnel
//! senders: an mDNS PTR query for `_jtunnel._udp.local` sent to mDNS's group
//! (224.0.0.251:5353) from the receive socket, so it is a legacy unicast query (RFC
//! 6762 §6.7) and each sender answers that socket directly. No socket joins the group
//! or binds port 5353. An answer from a LAN address's port 5353 goes to a
//! [`Directory`]: a sender is listed when it answers, dropped on its goodbye, and
//! forgotten 35 s after it last answered, at most [`MAX_LISTED`] at a time. Listing a
//! sender trusts it with nothing: only an accepted sender is subscribed to, and
//! `jtunnel.senders` reports the list. Each change to it is sent on the host's
//! `changed` channel, which the engine turns into `jtunnel.senders_changed`.
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

use crate::lan::is_lan;
use jstream_jtunnel::Playout;
use jstream_jtunnel::dnssd::{self, Directory};
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
/// How often the LAN is asked for senders: three times in the 35 s a listed sender
/// lasts unheard.
const QUERY_EVERY: Duration = Duration::from_secs(10);
/// How often senders not heard lately are forgotten.
const EXPIRE_EVERY: Duration = Duration::from_secs(1);
/// The most senders listed at a time, so a host on the LAN that answers with many names
/// can't grow the list without bound.
pub const MAX_LISTED: usize = 64;
/// mDNS's group and port, where the thread sends its queries.
pub const MDNS: SocketAddr = SocketAddr::new(
    IpAddr::V4(Ipv4Addr::new(
        dnssd::MDNS_GROUP[0],
        dnssd::MDNS_GROUP[1],
        dnssd::MDNS_GROUP[2],
        dnssd::MDNS_GROUP[3],
    )),
    dnssd::MDNS_PORT,
);

/// A sender the person accepted on this machine: its tunnel name, and the address and
/// port it sends from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedSender {
    name: String,
    addr: SocketAddr,
}

impl AcceptedSender {
    /// A sender called `name` at `addr`. An IPv4 address mapped into IPv6 is taken as
    /// itself, since the receive socket is IPv4.
    ///
    /// # Errors
    /// A name that is empty, longer than JTunnel's 31 bytes, holds a control character
    /// or starts or ends with white space (a source's `sender` is trimmed, so it could
    /// never match), or an address that is IPv6, outside the LAN ranges ([`is_lan`]) or
    /// with port 0.
    pub fn new(name: &str, addr: SocketAddr) -> Result<Self, String> {
        if name.is_empty()
            || name.len() > wire::MAX_TUNNEL_NAME
            || name.chars().any(char::is_control)
            || name.trim() != name
        {
            return Err(format!(
                "a JTunnel sender's name must be 1 to {} bytes with no control characters \
                 and no white space at either end",
                wire::MAX_TUNNEL_NAME
            ));
        }
        let addr = match addr.ip() {
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => SocketAddr::new(IpAddr::V4(v4), addr.port()),
                None => {
                    return Err(format!(
                        "{addr} is an IPv6 address: JTunnel is received on IPv4 only"
                    ));
                }
            },
            IpAddr::V4(_) => addr,
        };
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
    /// Where it asks for senders, [`MDNS`] unless the host names another (a test's
    /// responder); `None` asks nobody. Answers are read from this port.
    pub discover: Option<SocketAddr>,
}

impl JtunnelSetup {
    /// Receives from `accepted` on any interface, on a port the system picks.
    ///
    /// # Errors
    /// More than [`MAX_SENDERS`], or one name or one address and port twice: the
    /// receiver tells senders apart by where their datagrams come from.
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
            if let Some(other) = accepted[..n].iter().find(|s| same(s.addr, sender.addr)) {
                return Err(format!(
                    "the JTunnel senders `{}` and `{}` both send from {}",
                    other.name, sender.name, sender.addr
                ));
            }
        }
        Ok(Self {
            accepted,
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            discover: Some(MDNS),
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
    /// Datagrams that aren't a JTunnel packet it accepts, or an mDNS answer it can read.
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
    /// The open stream that records it; 0 for none. One source at a time.
    recorder: AtomicU64,
    /// Its playout as the thread last read it, for `profile.get` (#1223).
    seen: Seen,
}

/// A sender's playout as its thread last published it: each a plain counter or reading,
/// read one at a time, so a profile may mix two of the thread's turns.
#[derive(Default)]
struct Seen {
    recording: AtomicBool,
    held_ns: AtomicU64,
    lead_ns: AtomicU64,
    /// The drift's bits; 0 for none (0.0 drift is stored as -0.0).
    drift: AtomicU64,
    played: AtomicU64,
    concealed: AtomicU64,
    late: AtomicU64,
    repeats: AtomicU64,
    restarts: AtomicU64,
}

impl Seen {
    fn publish(&self, stream: &Stream, now_ns: u64) {
        let r = Ordering::Relaxed;
        let stats = stream.playout.stats();
        self.recording.store(stream.handler.is_some(), r);
        self.held_ns.store(stream.playout.held_ns(now_ns), r);
        self.lead_ns.store(stream.playout.lead_ns(), r);
        let drift = stream.playout.clock().drift_ppm().map_or(0, |ppm| {
            if ppm == 0.0 {
                (-0.0_f64).to_bits()
            } else {
                ppm.to_bits()
            }
        });
        self.drift.store(drift, r);
        self.played.store(stats.played, r);
        self.concealed.store(stats.concealed, r);
        self.late.store(stats.late, r);
        self.repeats.store(stats.repeats, r);
        self.restarts.store(stats.restarts, r);
    }
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
    directory: Arc<Mutex<Directory>>,
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
    /// Each change to the senders heard is sent on `changed`, without waiting: one
    /// message stands for any number of changes not yet taken.
    ///
    /// # Errors
    /// The socket could not be bound or set up, or the thread could not start.
    pub(crate) fn start(
        setup: &JtunnelSetup,
        clock: Arc<dyn MediaClock>,
        midi: Option<Arc<MidiInputs>>,
        changed: Option<SyncSender<()>>,
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
                recorder: AtomicU64::new(0),
                seen: Seen::default(),
            })
            .collect();
        let counters = Arc::new(JtunnelCounters::default());
        let directory = Arc::new(Mutex::new(Directory::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (commands, inbox) = sync_channel(COMMANDS);
        let thread = {
            let mut run = Run {
                socket,
                senders: Arc::clone(&senders),
                counters: Arc::clone(&counters),
                clock,
                inbox,
                directory: Arc::clone(&directory),
                discover: setup.discover,
                changed,
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
            directory,
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

    /// Its senders and counters for `profile.get` (#1223): counts since it started.
    pub(crate) fn profile(&self) -> jstream_protocol::NetworkProfile {
        let r = Ordering::Relaxed;
        let ms = |ns: u64| finite(ns as f32 / 1e6);
        let stats = self.stats();
        jstream_protocol::NetworkProfile {
            inputs: self
                .senders
                .iter()
                .map(|sender| {
                    let seen = &sender.seen;
                    let drift = seen.drift.load(r);
                    jstream_protocol::NetworkInputProfile {
                        sender: sender.accepted.name.clone(),
                        sample_rate: sender.rate.load(Ordering::Acquire),
                        recording: seen.recording.load(r),
                        latency_ms: ms(seen.held_ns.load(r)),
                        lead_ms: ms(seen.lead_ns.load(r)),
                        drift_ppm: (drift != 0)
                            .then(|| jstream_protocol::Finite32::new(f64::from_bits(drift) as f32))
                            .flatten(),
                        played: seen.played.load(r),
                        concealed: seen.concealed.load(r),
                        late: seen.late.load(r),
                        repeats: seen.repeats.load(r),
                        restarts: seen.restarts.load(r),
                    }
                })
                .collect(),
            outside_lan: stats.outside_lan,
            unaccepted: stats.unaccepted,
            malformed: stats.malformed,
            unsupported: stats.unsupported,
            midi: stats.midi,
        }
    }

    /// The senders heard on the LAN, for `jtunnel.senders`: ordered by name, then
    /// address, each with whether this machine accepts it.
    pub(crate) fn heard(&self) -> Vec<jstream_protocol::JtunnelSenderInfo> {
        let heard = self
            .directory
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .senders();
        heard
            .into_iter()
            .map(|sender| {
                let address = SocketAddr::V4(sender.address);
                jstream_protocol::JtunnelSenderInfo {
                    accepted: self
                        .senders
                        .iter()
                        .any(|s| s.accepted.name == sender.name && same(s.accepted.addr, address)),
                    remote_name: sender.name,
                    address: address.to_string(),
                }
            })
            .collect()
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
    /// past its jitter-free arrival, until the returned stream is dropped. One source
    /// records a sender at a time: a second one is refused until the first stream is
    /// dropped, so it shows as unavailable rather than take the sender over (and be
    /// taken over back on its own reopen).
    ///
    /// # Errors
    /// As for [`input_format`](Self::input_format), a sender another stream records, or
    /// the thread has stopped.
    pub(crate) fn open(
        &self,
        name: &str,
        latency_ms: u32,
        handler: Box<dyn AudioInputHandler>,
    ) -> Result<Box<dyn AudioStream>, String> {
        let format = self.input_format(name)?;
        let sender = self.sender(name)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let recorder = &self.senders[sender].recorder;
        if recorder
            .compare_exchange(0, id, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(format!(
                "another source already records the JTunnel sender `{name}`"
            ));
        }
        let opened = self.commands.try_send(Command::Open {
            sender,
            id,
            rate: format.sample_rate(),
            lead_ns: u64::from(latency_ms) * 1_000_000,
            handler,
        });
        if opened.is_err() {
            recorder.store(0, Ordering::Release);
            return Err("the JTunnel receiver is not taking inputs".to_owned());
        }
        Ok(Box::new(NetworkStream {
            format,
            senders: Arc::clone(&self.senders),
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
    senders: Arc<[Sender]>,
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
        // The sender is free for another source.
        let _ = self.senders[self.sender].recorder.compare_exchange(
            self.id,
            0,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
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
    directory: Arc<Mutex<Directory>>,
    discover: Option<SocketAddr>,
    changed: Option<SyncSender<()>>,
    stop: Arc<AtomicBool>,
    streams: Vec<Stream>,
}

impl Run {
    fn run(&mut self) {
        // One byte more than a packet may be, so a longer datagram shows as one.
        let mut datagram = [0_u8; wire::MAX_PACKET_BYTES + 1];
        let mut samples = [0.0_f32; wire::MAX_FRAME_SAMPLES as usize * wire::CHANNELS as usize];
        let mut next_subscribe = Instant::now();
        let mut next_query = Instant::now();
        let mut next_expire = Instant::now() + EXPIRE_EVERY;
        while !self.stop.load(Ordering::Acquire) {
            if Instant::now() >= next_subscribe {
                self.send_all(wire::write_subscribe);
                next_subscribe += SUBSCRIBE_EVERY;
                // After a stall, the next keepalive goes out now and then on time.
                next_subscribe = next_subscribe.max(Instant::now());
            }
            if Instant::now() >= next_query {
                self.query();
                next_query = Instant::now() + QUERY_EVERY;
            }
            if Instant::now() >= next_expire {
                self.expire();
                next_expire = Instant::now() + EXPIRE_EVERY;
            }
            self.commands();
            let _ = self.socket.set_read_timeout(Some(self.wait()));
            // An error is a timeout, or an ICMP port-unreachable from a sender not
            // running yet, reported on the next receive on some systems.
            if let Ok((len, from)) = self.socket.recv_from(&mut datagram) {
                if len > wire::MAX_PACKET_BYTES {
                    // Cut short by the buffer: never parsed as if it were whole.
                    self.counters.malformed.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.receive(&datagram[..len], from, &mut samples);
                }
            }
            self.play();
        }
        self.send_all(|out, _| wire::write_bye(out));
        // A receiver stopped or replaced (the host started JTunnel again) leaves no
        // source recording silently: each reopens on the receiver now running. Opens
        // still queued count too.
        self.commands();
        for (index, stream) in self.streams.iter_mut().enumerate() {
            if let Some(mut gone) = stream.handler.take() {
                gone.handler.on_error(&PlatformError::DeviceLost {
                    what: format!("the JTunnel sender `{}`", self.senders[index].accepted.name),
                    reason: "the JTunnel receiver stopped".into(),
                });
            }
        }
    }

    /// Asks the LAN for JTunnel senders, from the receive socket.
    fn query(&self) {
        let Some(to) = self.discover else {
            return;
        };
        let mut out = [0_u8; 64];
        if let Some(len) = dnssd::write_query(&mut out) {
            // A network with no route to the group fails here; the next query tries
            // again.
            let _ = self.socket.send_to(&out[..len], to);
        }
    }

    /// The listed senders as the list's clock counts: milliseconds of media time.
    fn now_ms(&self) -> u64 {
        self.now_ns() / 1_000_000
    }

    /// Forgets the senders not heard lately, and says so if any went.
    fn expire(&self) {
        let now = self.now_ms();
        let mut directory = self
            .directory
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let before = directory.len();
        directory.expire(now);
        let gone = directory.len() != before;
        drop(directory);
        if gone {
            self.changed();
        }
    }

    /// Tells the host the senders heard changed, without waiting.
    fn changed(&self) {
        if let Some(changed) = &self.changed {
            // Full means a change is already waiting to be taken; that one covers this.
            let _ = changed.try_send(());
        }
    }

    /// Takes an mDNS answer from `from`'s address: the senders it announces join the
    /// list, or leave it on a goodbye.
    fn discovered(&self, datagram: &[u8], from: Ipv4Addr) {
        let Ok(announcements) = dnssd::parse_announcements(datagram) else {
            self.counters.malformed.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if announcements.is_empty() {
            return;
        }
        let now = self.now_ms();
        let mut directory = self
            .directory
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let before = directory.senders();
        let room = MAX_LISTED.saturating_sub(directory.len());
        // A full list still takes refreshes and goodbyes of the senders on it.
        let mut fresh = 0;
        let taken: Vec<_> = announcements
            .into_iter()
            .filter(|a| {
                let known = before.iter().any(|s| {
                    s.address.port() == a.port
                        && *s.address.ip() == from
                        && s.name.eq_ignore_ascii_case(&a.name)
                });
                if known || a.ttl == 0 {
                    return true;
                }
                fresh += 1;
                fresh <= room
            })
            .collect();
        directory.observe(from, &taken, now);
        let after = directory.senders();
        drop(directory);
        let moved = before.len() != after.len()
            || before
                .iter()
                .zip(&after)
                .any(|(a, b)| a.name != b.name || a.address != b.address);
        if moved {
            self.changed();
        }
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
                        // `open` lets one stream record a sender at a time, so this
                        // replaces only a closed one whose close is still queued.
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
            if let Some(discover) = self.discover
                && from.port() == discover.port()
                && let Some(ip) = ipv4(from.ip())
            {
                self.discovered(datagram, ip);
                return;
            }
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
            self.senders[index].seen.publish(stream, now);
        }
    }
}

/// `ip` as IPv4, whether or not it came mapped into IPv6.
fn ipv4(ip: IpAddr) -> Option<Ipv4Addr> {
    match ip {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(v6) => v6.to_ipv4_mapped(),
    }
}

/// `value`, or 0 if it isn't finite.
fn finite(value: f32) -> jstream_protocol::Finite32 {
    jstream_protocol::Finite32::new(value).unwrap_or_default()
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
