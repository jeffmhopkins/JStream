//! PTZ controllers (ADR-0152, #1181): a joystick keyboard or a PTZ control app that
//! drives JStream's scene cameras over VISCA, as it drives a head.
//!
//! The host lists the endpoints and the controller addresses it accepts
//! ([`PtzControllerSetup`]); with none, no socket is open (ADR-0123 §1). Each endpoint
//! listens on one LAN interface and port, never on all of them, and answers only the
//! allowlisted controller addresses; anything else is dropped and counted (ADR-0123
//! §5). It is bound to one target, a scene or whatever is on program or preview, and is
//! an ordinary protocol client of the engine: it translates each VISCA command into
//! `scene.drive_camera` or `scene.aim_camera` for the target's camera, answers each
//! inquiry from `scene.camera_aim`, and runs nothing else. Its session's drives stop
//! when the endpoint stops, as any session's do.
//!
//! Presets live in the endpoint, in memory: "preset set" stores the target camera's aim
//! now, "preset recall" aims it back there. Nothing a controller sends is stored in the
//! collection or undone.
//!
//! A drive stops when its controller goes quiet (ADR-0153 §1): once the endpoint has
//! heard nothing from an allowed controller for [`IDLE_STOP`] while an axis it set
//! moving still moves, it stops those axes, as a TCP endpoint does when its last
//! connection closes. Each endpoint counts what it answered and dropped, and
//! `profile.get` reports them (`ptz_controllers`, ADR-0153 §2).

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::pin::pin;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use jstream_control::Control;
use jstream_protocol::{
    Action, Call, CallResult, CameraAim, CameraAimInfo, Finite32, Hello, MAX_AIM_PAN_DEG,
    MAX_AIM_TILT_DEG, MAX_AIM_ZOOM, MAX_PTZ_ZOOM_SPEED, PROTOCOL_VERSION, PtzControllerProfile,
    PtzTransport, Query, QueryResult, SceneId,
};
use jstream_visca::{
    Address, Command, Drive, ErrorKind, FocusMode, Inquiry, InquiryValue, Pan, PositionWidth,
    PresetOp, Reply, Request, Tilt, ip,
};

use crate::lan::is_lan;
use crate::session::InProcessControl;

#[cfg(test)]
mod tests;

/// How long a socket read waits before the thread checks whether to stop.
const TICK: Duration = Duration::from_millis(50);
/// The most TCP connections one endpoint holds at once.
const MAX_CONNECTIONS: usize = 4;
/// Pan and tilt counts a degree, in the positions a controller reads and sends: pan
/// ±170° is ±2720, tilt ±89° ±1424, so they fit both Sony's widths.
const COUNTS_PER_DEG: f64 = 16.0;
/// The zoom position at the most zoom; 0 is the stored camera's own field of view.
const MAX_ZOOM_COUNT: f64 = 16384.0;
/// The zoom step a standard-speed zoom (`8x 01 04 07 02 FF`) drives at, of 1 to 8.
const STANDARD_ZOOM_STEP: i8 = 4;
/// How long an endpoint waits, hearing nothing from an allowed controller, before it
/// stops the drives that controller left moving (ADR-0153 §1). Any message keeps a drive
/// going, an inquiry included, and a controller that holds its joystick still sends the
/// next one well inside this.
pub const IDLE_STOP: Duration = Duration::from_secs(5);

/// What a controller endpoint drives: one scene's camera, or the camera of whatever is
/// on program or on preview when each command arrives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PtzControllerTarget {
    /// The scene on program.
    Program,
    /// The scene on preview.
    Preview,
    /// This scene: a keyframe scene drives its family's camera.
    Scene(SceneId),
}

impl fmt::Display for PtzControllerTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Program => f.write_str("program"),
            Self::Preview => f.write_str("preview"),
            Self::Scene(id) => write!(f, "scene:{id}"),
        }
    }
}

/// One endpoint a controller drives (ADR-0152): where it listens, over which
/// transport, as which VISCA address, and what it drives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PtzController {
    bind: SocketAddr,
    transport: PtzTransport,
    visca: u8,
    target: PtzControllerTarget,
}

impl PtzController {
    /// An endpoint listening on `bind` over `transport`, answering as VISCA address
    /// `visca` (1 to 7), that drives `target`.
    ///
    /// # Errors
    /// A bind address that is every interface (`0.0.0.0` or `::`) or outside the LAN
    /// ranges ([`is_lan`]); a VISCA address outside 1 to 7.
    pub fn new(
        bind: SocketAddr,
        transport: PtzTransport,
        visca: u8,
        target: PtzControllerTarget,
    ) -> Result<Self, String> {
        if bind.ip().is_unspecified() {
            return Err(
                "a PTZ controller listens on one interface: give its address, not every \
                 interface (ADR-0123 §5)"
                    .into(),
            );
        }
        if !is_lan(bind.ip()) {
            return Err(format!(
                "{} is not a LAN address: loopback, 10/8, 172.16/12, 192.168/16, \
                 169.254/16 or ::1 (ADR-0123 §2)",
                bind.ip()
            ));
        }
        if Address::new(visca).is_none() {
            return Err("the VISCA address must be 1 to 7".into());
        }
        Ok(Self {
            bind,
            transport,
            visca,
            target,
        })
    }

    /// Where it listens.
    #[must_use]
    pub fn bind(&self) -> SocketAddr {
        self.bind
    }

    /// Its transport.
    #[must_use]
    pub fn transport(&self) -> PtzTransport {
        self.transport
    }

    /// What it drives.
    #[must_use]
    pub fn target(&self) -> &PtzControllerTarget {
        &self.target
    }
}

/// `address:port`, then optionally `/visca_ip`, `/udp` or `/tcp` (`visca_ip` by
/// default), then optionally `#n` for the VISCA address it answers as (1 by default),
/// then `=program`, `=preview` or `=scene:<scene id>`: the daemon's `--ptz-controller`.
impl FromStr for PtzController {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let (rest, target) = text.rsplit_once('=').ok_or(
            "expected address:port[/transport][#visca address]=program|preview|scene:<id>",
        )?;
        let target = match target {
            "program" => PtzControllerTarget::Program,
            "preview" => PtzControllerTarget::Preview,
            other => match other.strip_prefix("scene:") {
                Some(id) if !id.is_empty() => PtzControllerTarget::Scene(SceneId::new(id)),
                _ => {
                    return Err(format!(
                        "`{other}` is not a target: program, preview or scene:<id>"
                    ));
                }
            },
        };
        let (rest, visca) = match rest.rsplit_once('#') {
            Some((rest, n)) => (
                rest,
                n.parse::<u8>()
                    .map_err(|_| format!("`{n}` is not a VISCA address"))?,
            ),
            None => (rest, 1),
        };
        let (addr, transport) = match rest.rsplit_once('/') {
            Some((addr, t)) => (
                addr,
                match t {
                    "visca_ip" => PtzTransport::ViscaIp,
                    "udp" => PtzTransport::Udp,
                    "tcp" => PtzTransport::Tcp,
                    other => {
                        return Err(format!(
                            "`{other}` is not a transport: visca_ip, udp or tcp"
                        ));
                    }
                },
            ),
            None => (rest, PtzTransport::ViscaIp),
        };
        let bind: SocketAddr = addr
            .parse()
            .map_err(|_| format!("`{addr}` is not an address:port"))?;
        Self::new(bind, transport, visca, target)
    }
}

impl fmt::Display for PtzController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}#{}={}",
            self.bind,
            self.transport.name(),
            self.visca,
            self.target
        )
    }
}

/// The controller endpoints this machine opens and the controller addresses it accepts
/// (ADR-0152, ADR-0123 §5), for [`Engine::start_ptz_controllers`].
///
/// [`Engine::start_ptz_controllers`]: crate::Engine::start_ptz_controllers
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PtzControllerSetup {
    controllers: Vec<PtzController>,
    allow: Vec<IpAddr>,
}

impl PtzControllerSetup {
    /// The endpoints `controllers`, answering only the addresses in `allow`.
    ///
    /// # Errors
    /// An endpoint with no allowed address, an allowed address outside the LAN ranges,
    /// or two endpoints on one address, port and kind of socket (port 0, which the OS
    /// picks, excepted).
    pub fn new(controllers: Vec<PtzController>, allow: Vec<IpAddr>) -> Result<Self, String> {
        if !controllers.is_empty() && allow.is_empty() {
            return Err(
                "a PTZ controller endpoint answers only the controller addresses allowed: \
                 allow at least one (ADR-0123 §5)"
                    .into(),
            );
        }
        if let Some(ip) = allow.iter().find(|ip| !is_lan(**ip)) {
            return Err(format!(
                "{ip} is not a LAN address: loopback, 10/8, 172.16/12, 192.168/16, \
                 169.254/16 or ::1 (ADR-0123 §2)"
            ));
        }
        let tcp = |c: &PtzController| c.transport == PtzTransport::Tcp;
        for (i, c) in controllers.iter().enumerate() {
            if controllers[..i].iter().any(|earlier| {
                c.bind.port() != 0 && earlier.bind == c.bind && tcp(earlier) == tcp(c)
            }) {
                return Err(format!("two PTZ controller endpoints listen on {}", c.bind));
            }
        }
        Ok(Self { controllers, allow })
    }

    /// The endpoints, in order.
    #[must_use]
    pub fn controllers(&self) -> &[PtzController] {
        &self.controllers
    }

    /// The controller addresses allowed.
    #[must_use]
    pub fn allow(&self) -> &[IpAddr] {
        &self.allow
    }
}

/// What an endpoint counts from when it opened (ADR-0153 §2): the messages it
/// answered, what it dropped and why, and the drives it stopped for quiet.
#[derive(Debug, Default)]
struct Counters {
    answered: AtomicU64,
    unallowed: AtomicU64,
    malformed: AtomicU64,
    refused: AtomicU64,
    idle_stops: AtomicU64,
}

/// One running endpoint, as `profile.get` reports it: where it listens, what it
/// drives, and its counts. The engine holds it weakly, so it leaves the report when the
/// endpoints stop.
#[derive(Debug)]
pub(crate) struct EndpointStats {
    local: SocketAddr,
    transport: PtzTransport,
    target: String,
    counters: Counters,
}

impl EndpointStats {
    /// `profile.get`'s entry for it.
    pub(crate) fn profile(&self) -> PtzControllerProfile {
        let c = &self.counters;
        PtzControllerProfile {
            address: self.local.to_string(),
            transport: self.transport.name().to_owned(),
            target: self.target.clone(),
            answered: c.answered.load(Ordering::Relaxed),
            unallowed: c.unallowed.load(Ordering::Relaxed),
            malformed: c.malformed.load(Ordering::Relaxed),
            refused: c.refused.load(Ordering::Relaxed),
            idle_stops: c.idle_stops.load(Ordering::Relaxed),
        }
    }
}

/// The running endpoints, from [`Engine::start_ptz_controllers`]. Dropping it closes
/// every socket and ends every endpoint's session, so their drives stop.
///
/// [`Engine::start_ptz_controllers`]: crate::Engine::start_ptz_controllers
pub struct PtzControllers {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    endpoints: Vec<Arc<EndpointStats>>,
}

impl fmt::Debug for PtzControllers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PtzControllers")
            .field("endpoints", &self.local_addrs())
            .finish_non_exhaustive()
    }
}

impl PtzControllers {
    /// Each endpoint's bound address, in the setup's order: the port the OS chose for
    /// port 0.
    #[must_use]
    pub fn local_addrs(&self) -> Vec<SocketAddr> {
        self.endpoints.iter().map(|e| e.local).collect()
    }

    /// Each endpoint's counts, in the setup's order, as `profile.get` reports them.
    #[must_use]
    pub fn profiles(&self) -> Vec<PtzControllerProfile> {
        self.endpoints.iter().map(|e| e.profile()).collect()
    }

    /// How many messages the endpoints answered, in all.
    #[must_use]
    pub fn answered(&self) -> u64 {
        self.endpoints
            .iter()
            .map(|e| e.counters.answered.load(Ordering::Relaxed))
            .sum()
    }

    /// How many datagrams and connections the endpoints dropped, in all: from an
    /// address not allowed, not VISCA, or past the connection limit.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.endpoints
            .iter()
            .map(|e| {
                let c = &e.counters;
                c.unallowed.load(Ordering::Relaxed)
                    + c.malformed.load(Ordering::Relaxed)
                    + c.refused.load(Ordering::Relaxed)
            })
            .sum()
    }
}

impl Drop for PtzControllers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// An endpoint's socket.
enum Socket {
    Udp(UdpSocket),
    Tcp(TcpListener),
}

/// Opens `setup`'s endpoints, each with its own thread and session on `engine`.
pub(crate) fn start(
    engine: &crate::Engine,
    setup: &PtzControllerSetup,
) -> io::Result<PtzControllers> {
    // Every socket opens before any thread starts, so a port in use fails the whole
    // setup and leaves nothing listening.
    let mut sockets = Vec::new();
    for c in &setup.controllers {
        let socket = match c.transport {
            PtzTransport::ViscaIp | PtzTransport::Udp => {
                let socket = UdpSocket::bind(c.bind)?;
                socket.set_read_timeout(Some(TICK))?;
                Socket::Udp(socket)
            }
            PtzTransport::Tcp => {
                let listener = TcpListener::bind(c.bind)?;
                listener.set_nonblocking(true)?;
                Socket::Tcp(listener)
            }
        };
        let local = match &socket {
            Socket::Udp(s) => s.local_addr()?,
            Socket::Tcp(l) => l.local_addr()?,
        };
        sockets.push((c.clone(), socket, local));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let allow: Arc<[IpAddr]> = setup.allow.clone().into();
    let mut running = PtzControllers {
        stop: Arc::clone(&stop),
        threads: Vec::new(),
        endpoints: Vec::new(),
    };
    for (c, socket, local) in sockets {
        let stats = Arc::new(EndpointStats {
            local,
            transport: c.transport,
            target: c.target.to_string(),
            counters: Counters::default(),
        });
        let control = engine.connect();
        ready(control.hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: format!("PTZ controller {local}"),
        }))
        .map_err(|e| io::Error::other(e.to_string()))?;
        let endpoint = Endpoint {
            translator: Translator::new(control, &c),
            allow: Arc::clone(&allow),
            stats: Arc::clone(&stats),
            stop: Arc::clone(&stop),
            transport: c.transport,
        };
        let thread = thread::Builder::new()
            .name("jstream-ptz-in".into())
            .spawn(move || endpoint.run(socket))?;
        running.threads.push(thread);
        running.endpoints.push(stats);
    }
    engine.watch_ptz_controllers(&running.endpoints);
    Ok(running)
}

/// Polls `future` to its end. The engine's in-process calls are complete when made
/// ([`InProcessControl`]), so this never waits.
fn ready<F: Future>(future: F) -> F::Output {
    let mut cx = Context::from_waker(Waker::noop());
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(out) = future.as_mut().poll(&mut cx) {
            return out;
        }
        thread::yield_now();
    }
}

/// One endpoint's thread.
struct Endpoint {
    translator: Translator,
    allow: Arc<[IpAddr]>,
    stats: Arc<EndpointStats>,
    stop: Arc<AtomicBool>,
    transport: PtzTransport,
}

impl Endpoint {
    fn allowed(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            v4 => v4,
        };
        is_lan(ip) && self.allow.contains(&ip)
    }

    fn count(&self, counter: fn(&Counters) -> &AtomicU64) {
        counter(&self.stats.counters).fetch_add(1, Ordering::Relaxed);
    }

    /// Stops the drives a controller left moving once it has been quiet too long.
    fn check_idle(&mut self) {
        if self.translator.stop_if_idle(Instant::now()) {
            self.count(|c| &c.idle_stops);
        }
    }

    fn run(mut self, socket: Socket) {
        match socket {
            Socket::Udp(socket) => self.run_udp(&socket),
            Socket::Tcp(listener) => self.run_tcp(&listener),
        }
    }

    fn run_udp(&mut self, socket: &UdpSocket) {
        let mut buf = [0u8; 64];
        while !self.stop.load(Ordering::Relaxed) {
            let (n, from) = match socket.recv_from(&mut buf) {
                Ok(got) => got,
                Err(_) => {
                    // A timeout, or a datagram the OS refused: either way, a tick.
                    self.check_idle();
                    continue;
                }
            };
            if !self.allowed(from.ip()) {
                self.count(|c| &c.unallowed);
                self.check_idle();
                continue;
            }
            self.translator.heard(Instant::now());
            let datagram = &buf[..n];
            let mut out = [0u8; 32];
            if self.transport == PtzTransport::ViscaIp {
                let Ok(d) = ip::parse(datagram) else {
                    self.count(|c| &c.malformed);
                    continue;
                };
                match d.kind {
                    ip::PayloadType::Command | ip::PayloadType::Inquiry => {
                        for reply in self.translator.handle(d.payload) {
                            if let Ok(len) =
                                ip::write(&mut out, ip::PayloadType::Reply, d.seq, reply.as_slice())
                            {
                                let _ = socket.send_to(&out[..len], from);
                            }
                        }
                        self.count(|c| &c.answered);
                    }
                    // A controller resets the sequence numbers it sends with; any
                    // number is taken, so there is nothing to reset but the answer.
                    ip::PayloadType::Control if d.payload == ip::CONTROL_RESET => {
                        if let Ok(len) = ip::write(
                            &mut out,
                            ip::PayloadType::ControlReply,
                            d.seq,
                            &ip::CONTROL_RESET,
                        ) {
                            let _ = socket.send_to(&out[..len], from);
                        }
                        self.count(|c| &c.answered);
                    }
                    _ => self.count(|c| &c.malformed),
                }
            } else {
                for reply in self.translator.handle(datagram) {
                    let _ = socket.send_to(reply.as_slice(), from);
                }
                self.count(|c| &c.answered);
            }
        }
    }

    fn run_tcp(&mut self, listener: &TcpListener) {
        let mut streams: Vec<(TcpStream, Vec<u8>)> = Vec::new();
        while !self.stop.load(Ordering::Relaxed) {
            while let Ok((stream, from)) = listener.accept() {
                if !self.allowed(from.ip()) {
                    self.count(|c| &c.unallowed);
                    continue;
                }
                if streams.len() >= MAX_CONNECTIONS {
                    self.count(|c| &c.refused);
                    continue;
                }
                if stream.set_nonblocking(true).is_ok() {
                    self.translator.heard(Instant::now());
                    streams.push((stream, Vec::new()));
                }
            }
            let mut idle = true;
            let before = streams.len();
            let mut answered = 0;
            streams.retain_mut(|(stream, pending)| {
                let mut buf = [0u8; 64];
                match stream.read(&mut buf) {
                    Ok(0) => return false,
                    Ok(n) => {
                        idle = false;
                        self.translator.heard(Instant::now());
                        pending.extend_from_slice(&buf[..n]);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => return false,
                }
                while let Some(message) = ip::next_message(pending) {
                    let len = message.len();
                    for reply in self.translator.handle(message) {
                        if stream.write_all(reply.as_slice()).is_err() {
                            return false;
                        }
                    }
                    answered += 1;
                    pending.drain(..len);
                }
                true
            });
            self.stats
                .counters
                .answered
                .fetch_add(answered, Ordering::Relaxed);
            // The last connection closing is the controller going away: its drives stop
            // now rather than after the quiet (ADR-0153 §1).
            if before > 0 && streams.is_empty() && self.translator.stop_drives() {
                self.count(|c| &c.idle_stops);
            }
            self.check_idle();
            if idle {
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// A stored preset: the aim it recalls.
type Preset = (f32, f32, f32);

/// Turns VISCA messages into the endpoint session's calls, and their answers into
/// replies.
struct Translator {
    control: InProcessControl,
    target: PtzControllerTarget,
    visca: Address,
    /// The position width the controller sent last, which position answers use.
    width: PositionWidth,
    presets: BTreeMap<u8, Preset>,
    /// The scene whose camera this endpoint last drove, and which of its axes (pan,
    /// tilt, zoom) that drive left moving.
    driving: Option<(SceneId, [bool; 3])>,
    /// When an allowed controller was last heard.
    last_heard: Instant,
}

/// Why a message wasn't carried out: the reply is "not executable".
struct NotExecutable;

impl Translator {
    fn new(control: InProcessControl, c: &PtzController) -> Self {
        Self {
            control,
            target: c.target.clone(),
            visca: Address::new(c.visca).unwrap_or(Address::BROADCAST),
            width: PositionWidth::default(),
            presets: BTreeMap::new(),
            driving: None,
            last_heard: Instant::now(),
        }
    }

    /// An allowed controller was heard at `now`.
    fn heard(&mut self, now: Instant) {
        self.last_heard = now;
    }

    /// Stops the axes this endpoint's drive left moving, if its controller has been
    /// quiet for [`IDLE_STOP`] by `now`. Returns whether it stopped any.
    fn stop_if_idle(&mut self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_heard) >= IDLE_STOP && self.stop_drives()
    }

    /// Stops the axes this endpoint's drive left moving. Returns whether any were.
    fn stop_drives(&mut self) -> bool {
        let Some((scene_id, [pan, tilt, zoom])) = self.driving.take() else {
            return false;
        };
        let stop = |moving: bool| moving.then_some(0);
        // A camera that is gone has nothing left to stop.
        let _ = self.act(Action::SceneDriveCamera {
            scene_id,
            pan: stop(pan),
            tilt: stop(tilt),
            zoom: stop(zoom),
        });
        true
    }

    fn reply(&self, reply: Reply<'_>) -> Option<jstream_visca::Bytes> {
        reply.encode(self.visca).ok()
    }

    /// The replies to one VISCA message: an ACK and a completion for a command it
    /// carried out, the answer to an inquiry, an error for one it couldn't, and nothing
    /// for a message to another VISCA address.
    fn handle(&mut self, message: &[u8]) -> Vec<jstream_visca::Bytes> {
        let error = |socket, kind| Reply::Error { socket, kind };
        let (to, request) = match Request::parse(message) {
            Ok(parsed) => parsed,
            Err(jstream_visca::Error::Unsupported) => {
                return self
                    .reply(error(0, ErrorKind::NotExecutable))
                    .into_iter()
                    .collect();
            }
            Err(_) => {
                return self
                    .reply(error(0, ErrorKind::Syntax))
                    .into_iter()
                    .collect();
            }
        };
        if to != self.visca && to != Address::BROADCAST {
            return Vec::new();
        }
        match request {
            Request::Inquiry(inquiry) => match self.inquire(inquiry) {
                Ok(value) => value.encode(self.visca).ok().into_iter().collect(),
                Err(NotExecutable) => self
                    .reply(error(0, ErrorKind::NotExecutable))
                    .into_iter()
                    .collect(),
            },
            Request::Command(Command::IfClear) => self
                .reply(Reply::Completion { socket: 0 })
                .into_iter()
                .collect(),
            Request::Command(command) => {
                let ack = self.reply(Reply::Ack { socket: 1 });
                let end = match self.command(command) {
                    Ok(()) => self.reply(Reply::Completion { socket: 1 }),
                    Err(NotExecutable) => self.reply(error(1, ErrorKind::NotExecutable)),
                };
                ack.into_iter().chain(end).collect()
            }
        }
    }

    /// The scene the target names now.
    fn scene(&self) -> Result<SceneId, NotExecutable> {
        let (program, preview) = match &self.target {
            PtzControllerTarget::Scene(id) => return Ok(id.clone()),
            PtzControllerTarget::Program => (true, false),
            PtzControllerTarget::Preview => (false, true),
        };
        match ready(self.control.call(Call::Query {
            query: Query::StudioGet,
        })) {
            Ok(response) => match response.result {
                CallResult::Query {
                    result:
                        QueryResult::StudioGet {
                            program_scene,
                            preview_scene,
                            ..
                        },
                } => if program {
                    program_scene
                } else if preview {
                    preview_scene
                } else {
                    None
                }
                .ok_or(NotExecutable),
                _ => Err(NotExecutable),
            },
            Err(_) => Err(NotExecutable),
        }
    }

    fn act(&self, action: Action) -> Result<(), NotExecutable> {
        ready(self.control.call(Call::Action { action }))
            .map(|_| ())
            .map_err(|_| NotExecutable)
    }

    /// The target camera's aim now.
    fn aim(&self, scene_id: &SceneId) -> Result<CameraAimInfo, NotExecutable> {
        let response = ready(self.control.call(Call::Query {
            query: Query::SceneCameraAim {
                scene_id: scene_id.clone(),
            },
        }))
        .map_err(|_| NotExecutable)?;
        match response.result {
            CallResult::Query {
                result: QueryResult::SceneCameraAim { aim },
            } => Ok(aim),
            _ => Err(NotExecutable),
        }
    }

    fn drive(
        &mut self,
        pan: Option<i8>,
        tilt: Option<i8>,
        zoom: Option<i8>,
    ) -> Result<(), NotExecutable> {
        let scene_id = self.scene()?;
        self.act(Action::SceneDriveCamera {
            scene_id: scene_id.clone(),
            pan,
            tilt,
            zoom,
        })?;
        self.moved(
            &scene_id,
            [pan, tilt, zoom].map(|axis| axis.map(|step| step != 0)),
        );
        Ok(())
    }

    /// Notes which axes of `scene_id`'s camera now move from this endpoint's drive:
    /// `Some(true)` an axis it set moving, `Some(false)` one it stopped or aimed, `None`
    /// one it left alone. A drive on another scene's camera leaves the first one's to
    /// its own limits, as a head does.
    fn moved(&mut self, scene_id: &SceneId, axes: [Option<bool>; 3]) {
        let mut moving = match self.driving.take() {
            Some((driven, moving)) if driven == *scene_id => moving,
            _ => [false; 3],
        };
        for (now, set) in moving.iter_mut().zip(axes) {
            if let Some(set) = set {
                *now = set;
            }
        }
        if moving.iter().any(|m| *m) {
            self.driving = Some((scene_id.clone(), moving));
        }
    }

    fn aim_at(&mut self, aim: CameraAim, speed: Option<u8>) -> Result<(), NotExecutable> {
        let scene_id = self.scene()?;
        let set = [aim.pan_deg, aim.tilt_deg, aim.zoom].map(|axis| axis.is_some().then_some(false));
        self.act(Action::SceneAimCamera {
            scene_id: scene_id.clone(),
            aim,
            speed,
        })?;
        // A glide replaces the drive on the axes it sets, and ends by itself.
        self.moved(&scene_id, set);
        Ok(())
    }

    fn command(&mut self, command: Command) -> Result<(), NotExecutable> {
        let step = |speed: u8| i8::try_from(speed).unwrap_or(i8::MAX);
        match command {
            Command::PanTiltDrive {
                pan_speed,
                tilt_speed,
                pan,
                tilt,
            } => {
                let pan = match pan {
                    Pan::Left => -step(pan_speed),
                    Pan::Right => step(pan_speed),
                    Pan::Stop => 0,
                };
                let tilt = match tilt {
                    Tilt::Up => step(tilt_speed),
                    Tilt::Down => -step(tilt_speed),
                    Tilt::Stop => 0,
                };
                self.drive(Some(pan), Some(tilt), None)
            }
            Command::Zoom(drive) => {
                let zoom = match drive {
                    Drive::Stop => 0,
                    Drive::Plus(speed) => zoom_step(speed),
                    Drive::Minus(speed) => -zoom_step(speed),
                };
                self.drive(None, None, Some(zoom))
            }
            Command::PanTiltAbsolute {
                pan_speed,
                pan,
                tilt,
                width,
                ..
            } => {
                self.width = width;
                self.aim_at(
                    pan_tilt(
                        f64::from(pan) / COUNTS_PER_DEG,
                        f64::from(tilt) / COUNTS_PER_DEG,
                    ),
                    Some(pan_speed),
                )
            }
            Command::PanTiltRelative {
                pan_speed,
                pan,
                tilt,
                width,
                ..
            } => {
                self.width = width;
                let now = self.aim(&self.scene()?)?;
                self.aim_at(
                    pan_tilt(
                        f64::from(now.pan_deg.get()) + f64::from(pan) / COUNTS_PER_DEG,
                        f64::from(now.tilt_deg.get()) + f64::from(tilt) / COUNTS_PER_DEG,
                    ),
                    Some(pan_speed),
                )
            }
            // Home and a pan-tilt reset send pan and tilt back to the stored camera's
            // view; the zoom stays, as on a head.
            Command::Home | Command::PanTiltReset => self.aim_at(pan_tilt(0.0, 0.0), None),
            Command::ZoomDirect(count) => self.aim_at(
                CameraAim {
                    zoom: finite(zoom_of(count)),
                    ..CameraAim::default()
                },
                None,
            ),
            Command::Preset(op, n) => {
                match op {
                    PresetOp::Set => {
                        let now = self.aim(&self.scene()?)?;
                        self.presets
                            .insert(n, (now.pan_deg.get(), now.tilt_deg.get(), now.zoom.get()));
                    }
                    PresetOp::Reset => {
                        self.presets.remove(&n);
                    }
                    PresetOp::Recall => {
                        let (pan, tilt, zoom) = *self.presets.get(&n).ok_or(NotExecutable)?;
                        self.aim_at(
                            CameraAim {
                                pan_deg: Finite32::new(pan),
                                tilt_deg: Finite32::new(tilt),
                                zoom: Finite32::new(zoom),
                            },
                            None,
                        )?;
                    }
                }
                Ok(())
            }
            // A scene camera has no power and no focus motor: these are taken and do
            // nothing, so a controller that sends them as it connects carries on.
            Command::Power(_)
            | Command::Focus(_)
            | Command::FocusDirect(_)
            | Command::FocusMode(_)
            | Command::FocusOnePush
            | Command::IfClear => Ok(()),
        }
    }

    fn inquire(&self, inquiry: Inquiry) -> Result<InquiryValue, NotExecutable> {
        Ok(match inquiry {
            Inquiry::Power => InquiryValue::Power(true),
            Inquiry::FocusMode => InquiryValue::FocusMode(FocusMode::Auto),
            Inquiry::FocusPosition => InquiryValue::FocusPosition(0),
            Inquiry::ZoomPosition => {
                let aim = self.aim(&self.scene()?)?;
                InquiryValue::ZoomPosition(zoom_count(aim.zoom.get()))
            }
            Inquiry::PanTiltPosition => {
                let aim = self.aim(&self.scene()?)?;
                let count = |deg: f32| (f64::from(deg) * COUNTS_PER_DEG).round() as i32;
                InquiryValue::PanTiltPosition {
                    pan: count(aim.pan_deg.get()),
                    tilt: count(aim.tilt_deg.get()),
                    width: self.width,
                }
            }
        })
    }
}

fn finite(v: f64) -> Option<Finite32> {
    Finite32::new(v as f32)
}

/// An aim of pan and tilt, held to their limits.
fn pan_tilt(pan: f64, tilt: f64) -> CameraAim {
    let pan_max = f64::from(MAX_AIM_PAN_DEG);
    let tilt_max = f64::from(MAX_AIM_TILT_DEG);
    CameraAim {
        pan_deg: finite(pan.clamp(-pan_max, pan_max)),
        tilt_deg: finite(tilt.clamp(-tilt_max, tilt_max)),
        zoom: None,
    }
}

/// A variable zoom speed's step: VISCA's 0 to 7 as 1 to 8, or the standard step.
fn zoom_step(speed: Option<u8>) -> i8 {
    speed.map_or(STANDARD_ZOOM_STEP, |p| {
        i8::try_from(p.min(MAX_PTZ_ZOOM_SPEED - 1) + 1).unwrap_or(STANDARD_ZOOM_STEP)
    })
}

/// The zoom factor a zoom position stands for: 0 is 1, [`MAX_ZOOM_COUNT`] and above the
/// most zoom, evenly in its logarithm between.
fn zoom_of(count: u16) -> f64 {
    let k = (f64::from(count) / MAX_ZOOM_COUNT).min(1.0);
    (k * f64::from(MAX_AIM_ZOOM).ln()).exp()
}

/// The zoom position of a zoom factor.
fn zoom_count(zoom: f32) -> u16 {
    let k = f64::from(zoom.max(1.0)).ln() / f64::from(MAX_AIM_ZOOM).ln();
    (k.clamp(0.0, 1.0) * MAX_ZOOM_COUNT).round() as u16
}
