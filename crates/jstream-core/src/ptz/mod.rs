//! PTZ cameras (ADR-0146, #1182): the physical heads this machine drives over VISCA.
//!
//! The host gives the engine its camera list with [`Engine::start_ptz`]; with none, no
//! socket is open (ADR-0123 §1). Each camera gets a `jstream-ptz` thread with its own
//! socket, which sends what the `ptz.*` actions and the scenes' shots queue, waits for
//! each command's ACK (or error), and, while idle, asks the head's pan-tilt, zoom and
//! focus positions about twice a second. It reads only replies from the camera's own
//! address and port, which update only the pose and status `ptz.list` reports; anything
//! else is dropped and counted (ADR-0123 §4).
//!
//! The control side ([`Ptz`]) lives under the engine's lock and only sends on channels,
//! so an action never waits on the network.
//!
//! [`Engine::start_ptz`]: crate::Engine::start_ptz

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use jstream_graph::State;
use jstream_protocol::{
    Action, ClientId, MAX_PTZ_PAN_SPEED, MAX_PTZ_TILT_SPEED, ProtocolError, PtzCameraInfo,
    PtzFocus, PtzPose, PtzShot, PtzTransport, SceneId, ptz_action_check, ptz_camera_name_problem,
};
use jstream_visca::{
    Address, Command, Drive, ErrorKind, FocusMode, Inquiry, InquiryValue, Pan, PositionWidth,
    PresetOp, Reply, Tilt, ip,
};

use crate::lan::is_lan;
use crate::lock;

#[cfg(test)]
pub(crate) mod tests;

/// How long a command waits for its ACK, or an inquiry for its answer.
const REPLY_WAIT: Duration = Duration::from_millis(300);
/// How often an idle head is asked where it is.
const POLL_EVERY: Duration = Duration::from_millis(500);
/// A head that answered nothing for this long isn't reachable.
const REACHABLE_FOR: Duration = Duration::from_secs(3);
/// How long a TCP connection may take, and the wait before trying again.
const CONNECT_WAIT: Duration = Duration::from_secs(1);
/// How long a command that found the head's buffers full waits for one to complete.
const BUSY_WAIT: Duration = Duration::from_secs(2);
/// The most commands a head's thread holds unsent.
const MAX_QUEUE: usize = 16;
/// A drive that stops pan, tilt and zoom.
const STOP: Cmd = Cmd::Drive {
    pan: 0,
    tilt: 0,
    zoom: 0,
};

/// One camera on this machine's list: its name, LAN address, transport and VISCA
/// address (ADR-0146 §1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PtzCamera {
    name: String,
    addr: SocketAddr,
    transport: PtzTransport,
    visca: u8,
}

impl PtzCamera {
    /// A camera called `name` at `addr`, reached over `transport`, at VISCA address
    /// `visca` (1 to 7).
    ///
    /// # Errors
    /// A name that is empty, over 64 bytes or holds a control character; an address
    /// outside the LAN ranges ([`is_lan`]) or with port 0; a VISCA address outside 1 to 7.
    pub fn new(
        name: &str,
        addr: SocketAddr,
        transport: PtzTransport,
        visca: u8,
    ) -> Result<Self, String> {
        if let Some(reason) = ptz_camera_name_problem(name) {
            return Err(format!("the camera name {reason}"));
        }
        if !is_lan(addr.ip()) {
            return Err(format!(
                "{} is not a LAN address: loopback, 10/8, 172.16/12, 192.168/16, \
                 169.254/16 or ::1 (ADR-0123 §2)",
                addr.ip()
            ));
        }
        if addr.port() == 0 {
            return Err("the port must not be 0".into());
        }
        if Address::new(visca).is_none() {
            return Err("the VISCA address must be 1 to 7".into());
        }
        Ok(Self {
            name: name.to_owned(),
            addr,
            transport,
            visca,
        })
    }

    /// Its name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its address and port.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Its transport.
    #[must_use]
    pub fn transport(&self) -> PtzTransport {
        self.transport
    }

    /// Its VISCA address, 1 to 7.
    #[must_use]
    pub fn visca_address(&self) -> u8 {
        self.visca
    }

    fn visca(&self) -> Address {
        Address::new(self.visca).unwrap_or(Address::BROADCAST)
    }
}

/// `name@address:port`, then optionally `/visca_ip`, `/udp` or `/tcp` (`visca_ip` by
/// default), then optionally `#n` for the VISCA address (1 by default): the daemon's
/// `--ptz-camera`. An IPv6 address goes in brackets: `cam@[::1]:52381`.
impl FromStr for PtzCamera {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let (name, rest) = text
            .split_once('@')
            .ok_or("expected name@address:port[/transport][#visca address]")?;
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
        let addr: SocketAddr = addr
            .parse()
            .map_err(|_| format!("`{addr}` is not an address:port"))?;
        Self::new(name, addr, transport, visca)
    }
}

impl fmt::Display for PtzCamera {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}@{}/{}#{}",
            self.name,
            self.addr,
            self.transport.name(),
            self.visca
        )
    }
}

/// The cameras this machine controls (ADR-0146 §1), for [`Engine::start_ptz`].
///
/// [`Engine::start_ptz`]: crate::Engine::start_ptz
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PtzSetup {
    cameras: Vec<PtzCamera>,
}

impl PtzSetup {
    /// The list `cameras`, in order.
    ///
    /// # Errors
    /// Two cameras with one name, or two at one address, port and VISCA address, which
    /// would fight over one head.
    pub fn new(cameras: Vec<PtzCamera>) -> Result<Self, String> {
        for (i, camera) in cameras.iter().enumerate() {
            for earlier in &cameras[..i] {
                if earlier.name == camera.name {
                    return Err(format!("two PTZ cameras are called `{}`", camera.name));
                }
                if earlier.addr == camera.addr && earlier.visca == camera.visca {
                    return Err(format!(
                        "PTZ cameras `{}` and `{}` are the same head",
                        earlier.name, camera.name
                    ));
                }
            }
        }
        Ok(Self { cameras })
    }

    /// The cameras, in order.
    #[must_use]
    pub fn cameras(&self) -> &[PtzCamera] {
        &self.cameras
    }
}

/// What a head's thread sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cmd {
    Drive { pan: i8, tilt: i8, zoom: i8 },
    Aim { pose: PtzPose, speed: u8 },
    Home,
    Recall(u8),
    Store(u8),
    Focus(PtzFocus, Option<u8>),
}

/// What a head's thread reports.
#[derive(Clone, Debug, Default)]
struct Status {
    pose: PtzPose,
    answered: bool,
    last_reply: Option<Instant>,
    error: Option<String>,
    sent: u64,
    replies: u64,
    dropped: u64,
}

struct Shared {
    status: Mutex<Status>,
    stop: AtomicBool,
}

struct Head {
    camera: PtzCamera,
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
}

impl Drop for Head {
    fn drop(&mut self) {
        // The thread sees it at its next wait, at most a poll later, and closes its
        // socket.
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

/// The control side: one [`Head`] per camera, and the scenes it last framed.
#[derive(Default)]
pub(crate) struct Ptz {
    heads: Vec<Head>,
    program: Option<SceneId>,
    preview: Option<SceneId>,
    /// The client whose drive last set each camera moving, by camera name: its session
    /// ending stops the camera (ADR-0146 §3).
    drivers: Vec<(String, ClientId)>,
}

impl Ptz {
    /// Starts a thread per camera of `setup`, replacing (and stopping) any running. An
    /// empty setup stops them all.
    pub(crate) fn start(setup: &PtzSetup) -> io::Result<Self> {
        let mut heads = Vec::with_capacity(setup.cameras.len());
        for camera in &setup.cameras {
            let (tx, rx) = mpsc::channel();
            let shared = Arc::new(Shared {
                status: Mutex::new(Status::default()),
                stop: AtomicBool::new(false),
            });
            let link = Link::open(camera)?;
            let thread_shared = Arc::clone(&shared);
            let thread_camera = camera.clone();
            thread::Builder::new()
                .name("jstream-ptz".into())
                .spawn(move || Worker::new(thread_camera, link, thread_shared).run(&rx))?;
            heads.push(Head {
                camera: camera.clone(),
                tx,
                shared,
            });
        }
        Ok(Self {
            heads,
            program: None,
            preview: None,
            drivers: Vec::new(),
        })
    }

    fn head(&self, name: &str) -> Option<&Head> {
        self.heads.iter().find(|h| h.camera.name == name)
    }

    /// Runs a `ptz.*` action `client_id` sent: checks it, then queues it for its head.
    /// `None` for any other action. A drive that moves remembers its client, so the
    /// client's session ending stops it ([`Ptz::forget_client`]).
    pub(crate) fn act(
        &mut self,
        client_id: &ClientId,
        action: &Action,
    ) -> Option<Result<(), ProtocolError>> {
        let (camera, problem) = ptz_action_check(action)?;
        if let Some((field, reason)) = problem {
            return Some(Err(ProtocolError::invalid_argument(field, reason)));
        }
        if self.heads.is_empty() {
            return Some(Err(ProtocolError::ActionFailed {
                reason: "this machine controls no PTZ cameras".into(),
            }));
        }
        let Some(head) = self.head(camera) else {
            return Some(Err(ProtocolError::invalid_argument(
                "camera",
                format!("this machine has no PTZ camera called `{camera}`; `ptz.list` lists them"),
            )));
        };
        let cmd = match *action {
            Action::PtzDrive {
                pan, tilt, zoom, ..
            } => Cmd::Drive { pan, tilt, zoom },
            Action::PtzAim { pose, speed, .. } => Cmd::Aim {
                pose,
                speed: speed.unwrap_or(MAX_PTZ_PAN_SPEED),
            },
            Action::PtzHome { .. } => Cmd::Home,
            Action::PtzRecall { preset, .. } => Cmd::Recall(preset),
            Action::PtzStore { preset, .. } => Cmd::Store(preset),
            Action::PtzFocus { focus, speed, .. } => Cmd::Focus(focus, speed),
            _ => return None,
        };
        // A thread that is gone (it panicked) shows in `ptz.list` as unreachable.
        let _ = head.tx.send(cmd);
        if let Cmd::Drive { pan, tilt, zoom } = cmd {
            let name = head.camera.name.clone();
            self.drivers.retain(|(camera, _)| *camera != name);
            if (pan, tilt, zoom) != (0, 0, 0) {
                self.drivers.push((name, client_id.clone()));
            }
        }
        Some(Ok(()))
    }

    /// `client_id`'s session ended: stops every camera its drive left moving, so a client
    /// that disconnects or crashes mid-drive doesn't leave a head running to its end
    /// stop (ADR-0146 §3).
    pub(crate) fn forget_client(&mut self, client_id: &ClientId) {
        let (stop, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.drivers)
            .into_iter()
            .partition(|(_, driver)| driver == client_id);
        self.drivers = keep;
        for (camera, _) in stop {
            if let Some(head) = self.head(&camera) {
                let _ = head.tx.send(STOP);
            }
        }
    }

    /// `ptz.list`.
    pub(crate) fn list(&self) -> Vec<PtzCameraInfo> {
        let now = Instant::now();
        self.heads
            .iter()
            .map(|head| {
                let status = lock(&head.shared.status).clone();
                PtzCameraInfo {
                    camera: head.camera.name.clone(),
                    address: head.camera.addr.to_string(),
                    transport: head.camera.transport,
                    visca_address: head.camera.visca,
                    reachable: status
                        .last_reply
                        .is_some_and(|at| now.duration_since(at) < REACHABLE_FOR),
                    pose: status.answered.then_some(status.pose),
                    error: status.error,
                    sent: status.sent,
                    replies: status.replies,
                    dropped: status.dropped,
                }
            })
            .collect()
    }

    /// Fires the shots of the scenes the studio now shows (ADR-0146 §7): the program
    /// scene's when it changed, paced to `duration_ms` (the switch's transition), and the
    /// preview scene's when it changed, for the cameras the program's shots don't name.
    pub(crate) fn follow(
        &mut self,
        state: &State,
        program: Option<&SceneId>,
        preview: Option<&SceneId>,
        duration_ms: u32,
    ) {
        if self.heads.is_empty() {
            return;
        }
        let shots = |id: Option<&SceneId>| {
            id.and_then(|id| state.scene(id))
                .map(|scene| state.effective_ptz_shots(scene))
                .unwrap_or_default()
        };
        let live = shots(program);
        if program != self.program.as_ref() {
            self.program = program.cloned();
            for shot in &live {
                self.fire(shot, matched_speed(duration_ms));
            }
        }
        if preview != self.preview.as_ref() {
            self.preview = preview.cloned();
            for shot in shots(preview) {
                if !live.iter().any(|l| l.camera == shot.camera) {
                    self.fire(&shot, MAX_PTZ_PAN_SPEED);
                }
            }
        }
    }

    /// Queues `shot` for its head, if this machine lists the camera.
    fn fire(&self, shot: &PtzShot, speed: u8) {
        let Some(head) = self.head(&shot.camera) else {
            return;
        };
        let cmd = match shot.preset {
            Some(preset) => Cmd::Recall(preset),
            None => Cmd::Aim {
                pose: shot.pose,
                speed,
            },
        };
        let _ = head.tx.send(cmd);
    }
}

/// The pan speed a pose shot moves at when a switch with a transition of `duration_ms`
/// fires it (ADR-0146 §7): the fastest up to half a second, then proportionally slower,
/// never below the slowest step.
pub(crate) fn matched_speed(duration_ms: u32) -> u8 {
    if duration_ms <= 500 {
        return MAX_PTZ_PAN_SPEED;
    }
    let speed = (u32::from(MAX_PTZ_PAN_SPEED) * 500 + duration_ms / 2) / duration_ms;
    u8::try_from(speed.clamp(1, u32::from(MAX_PTZ_PAN_SPEED))).unwrap_or(1)
}

/// A head's socket.
enum Link {
    Udp(UdpSocket),
    Tcp {
        stream: Option<TcpStream>,
        buf: Vec<u8>,
        retry_at: Option<Instant>,
    },
}

impl Link {
    fn open(camera: &PtzCamera) -> io::Result<Self> {
        Ok(match camera.transport {
            PtzTransport::ViscaIp | PtzTransport::Udp => {
                let any: SocketAddr = if camera.addr.is_ipv4() {
                    ([0, 0, 0, 0], 0).into()
                } else {
                    (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
                };
                Link::Udp(UdpSocket::bind(any)?)
            }
            PtzTransport::Tcp => Link::Tcp {
                stream: None,
                buf: Vec::new(),
                retry_at: None,
            },
        })
    }
}

/// One head's thread.
struct Worker {
    camera: PtzCamera,
    link: Link,
    shared: Arc<Shared>,
    seq: ip::Sequence,
    width: PositionWidth,
    last_poll: Option<Instant>,
}

/// What a reply was, for the exchange waiting on it.
enum Got {
    Ack,
    Done,
    Data(InquiryValue),
    /// The head's command buffers are full.
    Full,
    Error(String),
}

impl Worker {
    fn new(camera: PtzCamera, link: Link, shared: Arc<Shared>) -> Self {
        Self {
            camera,
            link,
            shared,
            seq: ip::Sequence::default(),
            width: PositionWidth::default(),
            last_poll: None,
        }
    }

    fn status(&self) -> std::sync::MutexGuard<'_, Status> {
        lock(&self.shared.status)
    }

    fn run(mut self, rx: &Receiver<Cmd>) {
        if self.camera.transport == PtzTransport::ViscaIp {
            self.reset_sequence();
        }
        let mut queue: VecDeque<Cmd> = VecDeque::new();
        while !self.shared.stop.load(Ordering::Relaxed) {
            if queue.is_empty() {
                let wait = self
                    .last_poll
                    .map_or(Duration::ZERO, |at| POLL_EVERY.saturating_sub(at.elapsed()));
                match rx.recv_timeout(wait) {
                    Ok(cmd) => queue.push_back(cmd),
                    Err(RecvTimeoutError::Timeout) => {
                        self.poll();
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            for cmd in rx.try_iter() {
                enqueue(&mut queue, cmd);
            }
            if let Some(cmd) = queue.pop_front() {
                self.execute(cmd);
                // A command is followed by a poll, so the pose shown catches up.
                self.last_poll = Some(Instant::now() - POLL_EVERY + REPLY_WAIT);
            }
        }
        // Stopped or replaced: whatever a drive or a focus move started stops with it,
        // best effort, so a head never keeps moving to its end stop (ADR-0146 §3).
        self.execute(STOP);
        self.execute(Cmd::Focus(PtzFocus::Stop, None));
    }

    /// Asks where the head is: pan-tilt, zoom and focus.
    fn poll(&mut self) {
        self.last_poll = Some(Instant::now());
        for inquiry in [
            Inquiry::PanTiltPosition,
            Inquiry::ZoomPosition,
            Inquiry::FocusPosition,
        ] {
            if let Some(Got::Data(value)) = self.inquire(inquiry) {
                let mut status = self.status();
                status.answered = true;
                match value {
                    InquiryValue::PanTiltPosition { pan, tilt, width } => {
                        status.pose.pan = Some(pan);
                        status.pose.tilt = Some(tilt);
                        drop(status);
                        self.width = width;
                    }
                    InquiryValue::ZoomPosition(z) => status.pose.zoom = Some(z),
                    InquiryValue::FocusPosition(f) => status.pose.focus = Some(f),
                    InquiryValue::Power(_) | InquiryValue::FocusMode(_) => {}
                }
            }
        }
    }

    fn execute(&mut self, cmd: Cmd) {
        let commands = self.commands(cmd);
        let mut error = None;
        for command in commands {
            let mut got = self.command(command);
            // The head's command buffers are full (an aim's pan-tilt and zoom still
            // moving): wait for one to complete, then send it once more.
            if matches!(got, Some(Got::Full)) {
                self.await_completion();
                got = self.command(command);
            }
            match got {
                Some(Got::Error(why)) => error = Some(why),
                Some(Got::Full) => error = Some("the head's command buffers are full".into()),
                None => error = Some("no reply".to_owned()),
                Some(_) => {}
            }
        }
        self.status().error = error;
    }

    /// Reads replies until a completion arrives, for at most [`BUSY_WAIT`].
    fn await_completion(&mut self) {
        let deadline = Instant::now() + BUSY_WAIT;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if let Some((_, payload)) = self.receive(left)
                && let Ok((from, Reply::Completion { .. })) = Reply::parse(&payload)
                && from == self.camera.visca()
            {
                return;
            }
        }
    }

    /// Reads and drops any replies already waiting, so a late answer to an earlier
    /// message is never read as the next one's (raw VISCA has no sequence numbers).
    fn drain(&mut self) {
        for _ in 0..16 {
            if self.receive(Duration::ZERO).is_none() {
                return;
            }
        }
    }

    /// The VISCA commands `cmd` takes.
    fn commands(&self, cmd: Cmd) -> Vec<Command> {
        match cmd {
            Cmd::Drive { pan, tilt, zoom } => {
                fn dir<T>(v: i8, neg: T, pos: T, stop: T) -> T {
                    match v.signum() {
                        -1 => neg,
                        1 => pos,
                        _ => stop,
                    }
                }
                let speed = |v: i8, max: u8| v.unsigned_abs().clamp(1, max);
                vec![
                    Command::PanTiltDrive {
                        pan_speed: speed(pan, MAX_PTZ_PAN_SPEED),
                        tilt_speed: speed(tilt, MAX_PTZ_TILT_SPEED),
                        pan: dir(pan, Pan::Left, Pan::Right, Pan::Stop),
                        tilt: dir(tilt, Tilt::Down, Tilt::Up, Tilt::Stop),
                    },
                    Command::Zoom(match zoom.signum() {
                        1 => Drive::Plus(Some(zoom.unsigned_abs() - 1)),
                        -1 => Drive::Minus(Some(zoom.unsigned_abs() - 1)),
                        _ => Drive::Stop,
                    }),
                ]
            }
            Cmd::Aim { pose, speed } => {
                let mut out = Vec::new();
                if let (Some(pan), Some(tilt)) = (pose.pan, pose.tilt) {
                    out.push(Command::PanTiltAbsolute {
                        pan_speed: speed.clamp(1, MAX_PTZ_PAN_SPEED),
                        tilt_speed: speed.clamp(1, MAX_PTZ_TILT_SPEED),
                        pan,
                        tilt,
                        width: self.width,
                    });
                }
                if let Some(zoom) = pose.zoom {
                    out.push(Command::ZoomDirect(zoom));
                }
                if let Some(focus) = pose.focus {
                    out.push(Command::FocusMode(FocusMode::Manual));
                    out.push(Command::FocusDirect(focus));
                }
                out
            }
            Cmd::Home => vec![Command::Home],
            Cmd::Recall(p) => vec![Command::Preset(PresetOp::Recall, p)],
            Cmd::Store(p) => vec![Command::Preset(PresetOp::Set, p)],
            Cmd::Focus(focus, speed) => {
                let speed = speed.map(|s| s.clamp(1, 8) - 1);
                vec![match focus {
                    PtzFocus::Auto => Command::FocusMode(FocusMode::Auto),
                    PtzFocus::Manual => Command::FocusMode(FocusMode::Manual),
                    PtzFocus::OnePush => Command::FocusOnePush,
                    PtzFocus::Near => Command::Focus(Drive::Minus(speed)),
                    PtzFocus::Far => Command::Focus(Drive::Plus(speed)),
                    PtzFocus::Stop => Command::Focus(Drive::Stop),
                }]
            }
        }
    }

    /// Sends `command` and waits for its ACK, completion or error. `None` when nothing
    /// answered.
    fn command(&mut self, command: Command) -> Option<Got> {
        let bytes = match command.encode(self.camera.visca()) {
            Ok(bytes) => bytes,
            Err(err) => return Some(Got::Error(err.to_string())),
        };
        self.exchange(ip::PayloadType::Command, bytes.as_slice(), None)
    }

    /// Asks `inquiry` and waits for its answer. `None` when nothing (or an error)
    /// answered.
    fn inquire(&mut self, inquiry: Inquiry) -> Option<Got> {
        let bytes = inquiry.encode(self.camera.visca());
        self.exchange(ip::PayloadType::Inquiry, bytes.as_slice(), Some(inquiry))
            .filter(|got| matches!(got, Got::Data(_)))
    }

    /// Sends one message and reads replies until one answers it, for at most
    /// [`REPLY_WAIT`]. An inquiry waits for data; a command for its ACK or completion.
    /// Either takes an error.
    fn exchange(
        &mut self,
        kind: ip::PayloadType,
        message: &[u8],
        asked: Option<Inquiry>,
    ) -> Option<Got> {
        let inquiry = asked.is_some();
        if self.camera.transport != PtzTransport::ViscaIp {
            self.drain();
        }
        let seq = self.seq.take();
        if self.send(kind, seq, message).is_err() {
            return None;
        }
        let deadline = Instant::now() + REPLY_WAIT;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Some((reply_seq, payload)) = self.receive(left) else {
                continue;
            };
            if self.camera.transport == PtzTransport::ViscaIp && reply_seq != Some(seq) {
                // A late reply to an earlier message.
                continue;
            }
            let Ok((from, reply)) = Reply::parse(&payload) else {
                self.status().dropped += 1;
                continue;
            };
            if from != self.camera.visca() {
                self.status().dropped += 1;
                continue;
            }
            {
                let mut status = self.status();
                status.replies += 1;
                status.last_reply = Some(Instant::now());
            }
            match reply {
                Reply::Error {
                    kind: ErrorKind::BufferFull,
                    ..
                } => return Some(Got::Full),
                Reply::Error { kind, .. } => return Some(Got::Error(format!("{kind:?}"))),
                Reply::Ack { .. } if !inquiry => return Some(Got::Ack),
                Reply::Completion { .. } if !inquiry => return Some(Got::Done),
                Reply::Data(data) if inquiry => {
                    let value = asked.and_then(|q| InquiryValue::decode(q, data).ok());
                    return value.map(Got::Data);
                }
                // A completion of an earlier command, or data while a command waits.
                _ => {}
            }
        }
        None
    }

    fn send(&mut self, kind: ip::PayloadType, seq: u32, message: &[u8]) -> io::Result<()> {
        let mut datagram = [0u8; 32];
        let bytes: &[u8] = if self.camera.transport == PtzTransport::ViscaIp {
            let n = ip::write(&mut datagram, kind, seq, message)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
            &datagram[..n]
        } else {
            message
        };
        let addr = self.camera.addr;
        let sent = match &mut self.link {
            Link::Udp(socket) => socket.send_to(bytes, addr).map(|_| ()),
            Link::Tcp {
                stream,
                retry_at,
                buf,
            } => {
                if stream.is_none() && retry_at.is_none_or(|at| Instant::now() >= at) {
                    match TcpStream::connect_timeout(&addr, CONNECT_WAIT) {
                        Ok(s) => {
                            let _ = s.set_nodelay(true);
                            *stream = Some(s);
                            *retry_at = None;
                        }
                        Err(err) => {
                            *retry_at = Some(Instant::now() + CONNECT_WAIT);
                            return Err(err);
                        }
                    }
                }
                match stream {
                    Some(s) => s.write_all(bytes).inspect_err(|_| {
                        *stream = None;
                        buf.clear();
                    }),
                    None => Err(io::ErrorKind::NotConnected.into()),
                }
            }
        };
        if sent.is_ok() {
            self.status().sent += 1;
        }
        sent
    }

    /// One reply's sequence number (for VISCA over IP) and VISCA bytes, waiting at most
    /// `wait`. `None` on a timeout, or a datagram that was dropped.
    fn receive(&mut self, wait: Duration) -> Option<(Option<u32>, Vec<u8>)> {
        let wait = wait.max(Duration::from_millis(1));
        match &mut self.link {
            Link::Udp(socket) => {
                socket.set_read_timeout(Some(wait)).ok()?;
                let mut buf = [0u8; 64];
                let (n, from) = socket.recv_from(&mut buf).ok()?;
                if from != self.camera.addr {
                    self.status().dropped += 1;
                    return None;
                }
                let datagram = &buf[..n];
                if self.camera.transport == PtzTransport::Udp {
                    return Some((None, datagram.to_vec()));
                }
                let Ok(parsed) = ip::parse(datagram) else {
                    self.status().dropped += 1;
                    return None;
                };
                match parsed.kind {
                    ip::PayloadType::Reply => Some((Some(parsed.seq), parsed.payload.to_vec())),
                    ip::PayloadType::ControlReply => {
                        // A sequence error: reset the head's numbering and ours.
                        if parsed.payload == ip::CONTROL_ERROR_SEQUENCE {
                            self.reset_sequence();
                        }
                        None
                    }
                    _ => {
                        self.status().dropped += 1;
                        None
                    }
                }
            }
            Link::Tcp { stream, buf, .. } => {
                if let Some(message) = ip::next_message(buf) {
                    let message = message.to_vec();
                    buf.drain(..message.len());
                    return Some((None, message));
                }
                let Some(s) = stream else {
                    thread::sleep(wait);
                    return None;
                };
                s.set_read_timeout(Some(wait)).ok()?;
                let mut chunk = [0u8; 64];
                match s.read(&mut chunk) {
                    Ok(0) => {
                        *stream = None;
                        buf.clear();
                        None
                    }
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        let message = ip::next_message(buf)?.to_vec();
                        buf.drain(..message.len());
                        Some((None, message))
                    }
                    Err(err)
                        if matches!(
                            err.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        None
                    }
                    Err(_) => {
                        *stream = None;
                        buf.clear();
                        None
                    }
                }
            }
        }
    }

    /// VISCA over IP: resets the head's expected sequence number to 0 (its control
    /// command), as a controller does when it starts.
    fn reset_sequence(&mut self) {
        let mut datagram = [0u8; 16];
        if let Ok(n) = ip::write(
            &mut datagram,
            ip::PayloadType::Control,
            0,
            &ip::CONTROL_RESET,
        ) && let Link::Udp(socket) = &self.link
            && socket.send_to(&datagram[..n], self.camera.addr).is_ok()
        {
            self.status().sent += 1;
        }
        self.seq.reset();
    }
}

/// Adds `cmd` to the queue, which never builds a backlog (ADR-0146 §3): a drive
/// replaces an unsent drive in its place, and a stop also goes first; an aim replaces an
/// unsent aim; and past [`MAX_QUEUE`] the oldest unsent command is dropped.
fn enqueue(queue: &mut VecDeque<Cmd>, cmd: Cmd) {
    match cmd {
        STOP => {
            queue.retain(|queued| !matches!(queued, Cmd::Drive { .. }));
            queue.push_front(cmd);
            return;
        }
        Cmd::Drive { .. } | Cmd::Aim { .. } => {
            let same =
                |queued: &Cmd| std::mem::discriminant(queued) == std::mem::discriminant(&cmd);
            if let Some(slot) = queue.iter_mut().find(|queued| same(queued)) {
                *slot = cmd;
                return;
            }
        }
        _ => {}
    }
    if queue.len() >= MAX_QUEUE {
        queue.pop_front();
    }
    queue.push_back(cmd);
}
