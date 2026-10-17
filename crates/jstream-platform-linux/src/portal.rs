//! Screen and window capture through the xdg-desktop-portal ScreenCast interface
//! (ADR-0060 §3).
//!
//! The desktop's own picker chooses what is captured, so [`PortalCapture`] lists one
//! target per kind rather than every display and window. Starting one runs the portal's
//! session flow over the session bus (`handshake`, on the crate's own D-Bus client),
//! then reads the granted PipeWire node as a video stream of CPU frames.
//!
//! This module also holds the platform-independent parts of the protocol: the option
//! values `SelectSources` takes, request handle tokens and the request object path a
//! reply arrives on.

#[cfg(any(target_os = "linux", test))]
mod handshake;

/// The `types` option of `SelectSources`: which kinds of source the picker offers. A
/// bit set, as the portal defines it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceTypes(pub u32);

impl SourceTypes {
    /// A monitor.
    pub const MONITOR: Self = Self(1);
    /// A window.
    pub const WINDOW: Self = Self(2);
    /// A virtual monitor the compositor creates for the cast.
    pub const VIRTUAL: Self = Self(4);

    /// Whether every bit of `other` is set here.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// The `cursor_mode` option of `SelectSources`, and the portal's `AvailableCursorModes`
/// property, as a bit set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CursorModes(pub u32);

impl CursorModes {
    /// The cursor is not in the stream.
    pub const HIDDEN: Self = Self(1);
    /// The cursor is drawn into the frames.
    pub const EMBEDDED: Self = Self(2);
    /// The cursor is sent as PipeWire stream metadata, beside the frames.
    pub const METADATA: Self = Self(4);

    /// The mode to ask for, out of the modes the portal offers: metadata, so the source
    /// can show or hide the cursor itself, else embedded, else hidden.
    pub fn preferred(available: Self) -> Self {
        [Self::METADATA, Self::EMBEDDED]
            .into_iter()
            .find(|m| available.0 & m.0 != 0)
            .unwrap_or(Self::HIDDEN)
    }
}

/// The `persist_mode` option of `SelectSources` (interface version 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum PersistMode {
    /// The grant ends with the session.
    None = 0,
    /// The grant lasts while JStream runs.
    WhileRunning = 1,
    /// The grant lasts until the user revokes it: what ADR-0060 asks for, so a collection
    /// reopens on the same screen without a prompt.
    UntilRevoked = 2,
}

/// A request's `handle_token`: a fresh path element per request, so its `Response`
/// signal can be matched before the call returns. Only `[A-Za-z0-9_]`.
pub fn handle_token(serial: u64) -> String {
    format!("jstream_{serial}")
}

/// The object path a request's `Response` signal arrives on, for the caller's D-Bus
/// unique name (such as `:1.42`) and the request's `handle_token`:
/// `/org/freedesktop/portal/desktop/request/1_42/<token>`. `None` when the name isn't a
/// unique name or the token has characters a path element can't hold.
pub fn request_path(unique_name: &str, token: &str) -> Option<String> {
    let sender = unique_name.strip_prefix(':')?;
    let element_ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
    };
    if !element_ok(sender) || !element_ok(token) || token.contains('.') {
        return None;
    }
    Some(format!(
        "/org/freedesktop/portal/desktop/request/{}/{token}",
        sender.replace('.', "_")
    ))
}

/// The kind of target the user picks in the portal's dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PortalPick {
    /// A display.
    Display,
    /// A window.
    Window,
}

impl PortalPick {
    /// The device ID [`PortalCapture`](crate::PortalCapture) lists this pick under.
    pub fn device_id(self) -> &'static str {
        match self {
            Self::Display => "portal:display",
            Self::Window => "portal:window",
        }
    }

    /// The picker's source type for this pick.
    pub fn source_types(self) -> SourceTypes {
        match self {
            Self::Display => SourceTypes::MONITOR,
            Self::Window => SourceTypes::WINDOW,
        }
    }
}

#[cfg(target_os = "linux")]
pub use backend::PortalCapture;

#[cfg(target_os = "linux")]
mod backend {
    use std::fs;
    use std::io::Write;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use jstream_platform::{
        Capture, CaptureStats, CaptureStream, CaptureTarget, CaptureTargetInfo, DeviceId,
        GpuContext, PlatformError, VideoSurface,
    };

    use super::PortalPick;
    use super::handshake::{
        self, DESTINATION, Granted, Handshake, Next, PortalInfo, QUICK, REQUEST, SESSION,
    };
    use crate::dbus::bus::{BUS, Bus};
    use crate::dbus::{add_match, match_rule};
    use crate::pipewire::stream::{Shared, VideoStream};
    use crate::{PipeWire, Session, SessionKind, gpu};

    /// The portal's restore tokens, one per kind of pick: in memory, and in files in a
    /// folder when one is given, so a grant survives restarts (ADR-0060 §3).
    #[derive(Debug, Default)]
    struct RestoreTokens {
        dir: Option<PathBuf>,
        memory: Mutex<[Option<String>; 2]>,
    }

    fn slot(pick: PortalPick) -> usize {
        match pick {
            PortalPick::Display => 0,
            PortalPick::Window => 1,
        }
    }

    /// Whether `token` is one the portal could have issued: printable ASCII, short.
    /// Anything else in the file is ignored.
    fn plausible(token: &str) -> bool {
        !token.is_empty() && token.len() <= 256 && token.bytes().all(|b| b.is_ascii_graphic())
    }

    impl RestoreTokens {
        fn file(&self, pick: PortalPick) -> Option<PathBuf> {
            let name = match pick {
                PortalPick::Display => "screencast-display.token",
                PortalPick::Window => "screencast-window.token",
            };
            self.dir.as_ref().map(|d| d.join(name))
        }

        fn get(&self, pick: PortalPick) -> Option<String> {
            if let Ok(m) = self.memory.lock()
                && let Some(t) = &m[slot(pick)]
            {
                return Some(t.clone());
            }
            let text = read_private(&self.file(pick)?)?;
            let token = text.trim();
            plausible(token).then(|| token.to_owned())
        }

        /// Keeps `token`, replacing the spent one. Writing the file is best effort: a
        /// failure costs only a prompt next time. The token goes to a fresh file created
        /// 0600, renamed over the old one, so the file is only ever the user's whatever
        /// mode or owner an old one had, and a reader never sees half a token.
        fn put(&self, pick: PortalPick, token: &str) {
            if !plausible(token) {
                return;
            }
            if let Ok(mut m) = self.memory.lock() {
                m[slot(pick)] = Some(token.to_owned());
            }
            let (Some(dir), Some(file)) = (&self.dir, self.file(pick)) else {
                return;
            };
            let mut tmp = file.clone().into_os_string();
            tmp.push(".tmp");
            let tmp = PathBuf::from(tmp);
            let written = fs::create_dir_all(dir).and_then(|()| {
                match fs::remove_file(&tmp) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
                let mut f = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&tmp)?;
                f.write_all(token.as_bytes())?;
                fs::rename(&tmp, &file)
            });
            if written.is_err() {
                let _ = fs::remove_file(&tmp);
            }
        }
    }

    /// The file at `path`, only when it is a regular file (not a symlink) that this
    /// user owns and no one else can read or write. A token file anyone else could
    /// read has leaked, and one anyone else could write may be planted.
    fn read_private(path: &std::path::Path) -> Option<String> {
        use std::io::Read;
        use std::os::unix::fs::MetadataExt;

        let mut f = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .ok()?;
        let meta = f.metadata().ok()?;
        let me = fs::metadata("/proc/self").ok()?.uid();
        if !meta.is_file() || meta.uid() != me || meta.mode() & 0o077 != 0 {
            return None;
        }
        let mut text = String::new();
        f.read_to_string(&mut text).ok()?;
        Some(text)
    }

    /// Display and window capture through the ScreenCast portal (ADR-0060 §3).
    ///
    /// `start` blocks while the desktop's picker is open, up to five minutes; with a
    /// restore token from an earlier grant the portal skips the picker. Frames are CPU
    /// copies of PipeWire's shared-memory buffers, BGRA or RGBA. On a context
    /// [`LinuxGpu`](crate::LinuxGpu) opened with DMA-BUF import
    /// ([`on_context`](Capture::on_context)), they are textures on its device instead,
    /// RGBA, copied from PipeWire's DMA-BUFs; a stream whose DMA-BUFs don't import falls
    /// back to CPU frames.
    #[derive(Debug)]
    pub struct PortalCapture {
        session: Session,
        /// Shared with the backend on a GPU context, so both keep the same grants.
        tokens: Arc<RestoreTokens>,
        /// The context whose device the captures import into, on the GPU path.
        gpu: Option<Arc<gpu::Shared>>,
    }

    impl PortalCapture {
        /// A capture backend for `session`. A headless session lists no targets.
        /// Restore tokens are kept in memory only, so each run prompts once per kind.
        pub fn new(session: Session) -> Self {
            Self {
                session,
                tokens: Arc::default(),
                gpu: None,
            }
        }

        /// As [`new`](Self::new), keeping restore tokens in files in `dir` (such as a
        /// folder in `jstream_platform::paths::data_dir`), readable only by the user,
        /// so a grant survives restarts.
        pub fn with_token_dir(session: Session, dir: PathBuf) -> Self {
            Self {
                session,
                tokens: Arc::new(RestoreTokens {
                    dir: Some(dir),
                    memory: Mutex::default(),
                }),
                gpu: None,
            }
        }

        fn pick(target: &CaptureTarget) -> Option<PortalPick> {
            match target {
                CaptureTarget::Display(id) if id.as_str() == PortalPick::Display.device_id() => {
                    Some(PortalPick::Display)
                }
                CaptureTarget::Window(id) if id.as_str() == PortalPick::Window.device_id() => {
                    Some(PortalPick::Window)
                }
                _ => None,
            }
        }
    }

    fn no_answer(call: &'static str, message: &str) -> PlatformError {
        PlatformError::Os {
            call,
            code: None,
            message: message.into(),
        }
    }

    /// Runs the handshake to the end, returning the grant and the PipeWire socket.
    fn run(bus: &mut Bus, h: &mut Handshake) -> Result<(Granted, OwnedFd), PlatformError> {
        let portal = h.portal().to_owned();
        let from = Some(portal.as_str());
        loop {
            match h.next()? {
                Next::Request {
                    call,
                    what,
                    timeout,
                } => {
                    let reply = bus.call(call, from, what, QUICK)?;
                    h.on_handle(&reply.msg);
                    let deadline = Instant::now() + timeout;
                    let response = bus
                        .wait_signal(deadline, |m| h.is_response(m))?
                        .ok_or_else(|| {
                            no_answer(what, "no answer from the desktop portal in time")
                        })?;
                    h.on_response(&response)?;
                }
                Next::OpenRemote { call, what } => {
                    let reply = bus.call(call, from, what, QUICK)?;
                    let granted = h.granted(&reply.msg)?;
                    let fd = reply.fds.into_iter().next().ok_or_else(|| {
                        no_answer(what, "the PipeWire file descriptor didn't arrive")
                    })?;
                    return Ok((granted, fd));
                }
            }
        }
    }

    /// Closes the handshake's session, if it got one. Best effort: the session also
    /// ends when the connection closes.
    fn close(bus: &mut Bus, session: Option<&str>) {
        if let Some(s) = session {
            let _ = bus.send(&handshake::close_session(s));
        }
    }

    impl Capture for PortalCapture {
        fn targets(&self) -> Result<Vec<CaptureTargetInfo>, PlatformError> {
            if self.session.kind == SessionKind::Headless {
                return Ok(Vec::new());
            }
            Ok(vec![
                CaptureTargetInfo {
                    target: CaptureTarget::Display(DeviceId::new(PortalPick::Display.device_id())),
                    name: "A display, chosen in the system picker".into(),
                    size: None,
                    primary: false,
                },
                CaptureTargetInfo {
                    target: CaptureTarget::Window(DeviceId::new(PortalPick::Window.device_id())),
                    name: "A window, chosen in the system picker".into(),
                    size: None,
                    primary: false,
                },
            ])
        }

        fn start(&self, target: &CaptureTarget) -> Result<Box<dyn CaptureStream>, PlatformError> {
            let Some(pick) = Self::pick(target) else {
                let (kind, id) = match target {
                    CaptureTarget::Display(id) => ("display", id.as_str()),
                    CaptureTarget::Window(id) => ("window", id.as_str()),
                    _ => ("capture target", ""),
                };
                return Err(PlatformError::NotFound {
                    kind,
                    id: id.to_owned(),
                });
            };
            if self.session.kind == SessionKind::Headless {
                return Err(PlatformError::Unsupported {
                    what: "screen capture in a session with no display".into(),
                });
            }
            // A missing PipeWire fails before the user is shown a picker.
            let pw = PipeWire::load()?;
            let mut bus = Bus::session()?;
            // The first call starts the portal if it isn't running; then its owner is
            // known, and only that connection's replies and signals count, so another
            // process on the session bus can't answer for it.
            let get_all = "org.freedesktop.DBus.Properties.GetAll";
            let info = bus.call(PortalInfo::query(), None, get_all, QUICK)?;
            let owner = bus.call(
                handshake::portal_owner_query(),
                Some(BUS),
                "org.freedesktop.DBus.GetNameOwner",
                QUICK,
            )?;
            let portal = owner
                .msg
                .body
                .first()
                .and_then(crate::dbus::Value::as_str)
                .filter(|name| name.starts_with(':'))
                .map(str::to_owned)
                .ok_or_else(|| no_answer(get_all, "the portal's bus name has no owner"))?;
            if info.msg.sender.as_deref() != Some(portal.as_str()) {
                return Err(no_answer(get_all, "the reply didn't come from the portal"));
            }
            let info = PortalInfo::from_reply(&info.msg.body)?;
            for rule in [
                match_rule(DESTINATION, REQUEST, "Response"),
                match_rule(DESTINATION, SESSION, "Closed"),
            ] {
                bus.call(
                    add_match(&rule),
                    Some(BUS),
                    "org.freedesktop.DBus.AddMatch",
                    QUICK,
                )?;
            }
            let mut h = Handshake::new(
                pick,
                info,
                bus.unique_name(),
                &portal,
                self.tokens.get(pick),
            )?;
            let (granted, fd) = match run(&mut bus, &mut h) {
                Ok(done) => done,
                Err(e) => {
                    close(&mut bus, h.session());
                    return Err(e);
                }
            };
            if let Some(token) = &granted.restore_token {
                self.tokens.put(pick, token);
            }
            let shared = Arc::new(Shared::new());
            let node = granted.streams[0].node_id;
            let video = match VideoStream::connect(
                pw.library(),
                fd,
                node,
                Arc::clone(&shared),
                self.gpu.clone(),
            ) {
                Ok(v) => v,
                Err(e) => {
                    close(&mut bus, Some(&granted.session));
                    return Err(e);
                }
            };
            let watcher = Watcher::spawn(bus, granted.session, portal, Arc::clone(&shared))?;
            Ok(Box::new(PortalStream {
                video: Some(video),
                watcher: Some(watcher),
                shared,
                pick,
                _pw: pw,
            }))
        }

        /// This backend on `context`'s device, when [`LinuxGpu`](crate::LinuxGpu)
        /// opened it with DMA-BUF import; `Ok(None)` when the device imports none.
        fn on_context(
            &self,
            context: &GpuContext,
        ) -> Result<Option<Arc<dyn Capture>>, PlatformError> {
            let shared = gpu::Shared::of(context, "PortalCapture::on_context")?;
            if !shared.imports_dma_bufs() {
                return Ok(None);
            }
            Ok(Some(Arc::new(Self {
                session: self.session.clone(),
                tokens: Arc::clone(&self.tokens),
                gpu: Some(shared),
            })))
        }
    }

    /// Keeps the portal session's connection open (the session ends with it) and
    /// watches for the portal closing the session: the user stopped sharing, or
    /// revoked the grant.
    #[derive(Debug)]
    struct Watcher {
        stop: Arc<AtomicBool>,
        thread: JoinHandle<()>,
    }

    impl Watcher {
        fn spawn(
            mut bus: Bus,
            session: String,
            portal: String,
            shared: Arc<Shared>,
        ) -> Result<Self, PlatformError> {
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let thread = std::thread::Builder::new()
                .name("jstream-portal".into())
                .spawn(move || {
                    while !flag.load(Ordering::Acquire) {
                        let deadline = Instant::now() + Duration::from_millis(200);
                        match bus.wait_signal(deadline, |m| {
                            handshake::is_session_closed(m, &session, &portal)
                        }) {
                            Ok(None) => {}
                            Ok(Some(_)) => {
                                shared.lose("screen sharing was stopped in the desktop");
                                return;
                            }
                            Err(e) => {
                                shared.lose(&format!("the desktop portal went away: {e}"));
                                return;
                            }
                        }
                    }
                    close(&mut bus, Some(&session));
                })
                .map_err(|e| PlatformError::Os {
                    call: "spawning the portal thread",
                    code: e.raw_os_error().and_then(|c| u32::try_from(c).ok()),
                    message: e.to_string(),
                })?;
            Ok(Self { stop, thread })
        }

        fn stop(self) {
            self.stop.store(true, Ordering::Release);
            let _ = self.thread.join();
        }
    }

    /// A running portal capture.
    #[derive(Debug)]
    struct PortalStream {
        video: Option<VideoStream>,
        watcher: Option<Watcher>,
        shared: Arc<Shared>,
        pick: PortalPick,
        _pw: PipeWire,
    }

    impl CaptureStream for PortalStream {
        fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError> {
            if let Some(frame) = self.shared.mailbox.take() {
                return Ok(Some(frame));
            }
            match self.shared.lost() {
                Some(reason) => Err(PlatformError::DeviceLost {
                    what: format!("{:?} capture", self.pick),
                    reason,
                }),
                None => Ok(None),
            }
        }

        fn stats(&self) -> CaptureStats {
            CaptureStats {
                frames_captured: self.shared.captured.load(Ordering::Relaxed),
                frames_skipped: self.shared.skipped.load(Ordering::Relaxed),
                frames_undecodable: 0,
            }
        }
    }

    impl Drop for PortalStream {
        fn drop(&mut self) {
            // The stream first, so no frame arrives after the session closes. Stopping
            // the watcher waits up to one poll (200 ms) for its thread.
            drop(self.video.take());
            if let Some(w) = self.watcher.take() {
                w.stop();
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::fs::PermissionsExt;

        use jstream_platform::{AdapterId, Capture, GpuContext, GpuInterop, PlatformError};

        use super::{PortalPick, RestoreTokens, plausible};
        use crate::gpu::LinuxGpu;
        use crate::{PortalCapture, Session};

        /// A context another platform opened is refused; one of ours without DMA-BUF
        /// import (a software adapter, here) leaves the capture on CPU frames.
        #[test]
        fn the_gpu_path_needs_our_context_with_dma_buf_import() {
            let capture = PortalCapture::new(Session::from_env());
            let ours = match LinuxGpu::with_software().open_device() {
                Ok(context) => context,
                Err(PlatformError::Unsupported { .. }) => return,
                Err(e) => panic!("{e}"),
            };
            let on_gpu = capture.on_context(&ours).expect("our context");
            let imports = crate::gpu::Shared::of(&ours, "test")
                .expect("ours")
                .imports_dma_bufs();
            assert_eq!(on_gpu.is_some(), imports);
            let foreign = GpuContext::new(
                ours.adapter().clone(),
                ours.device().clone(),
                ours.queue().clone(),
                AdapterId {
                    luid: None,
                    vendor: 0,
                },
                None,
            );
            assert!(matches!(
                capture.on_context(&foreign),
                Err(PlatformError::InvalidState { .. })
            ));
        }

        #[test]
        fn restore_tokens_persist_per_pick() {
            let dir =
                std::env::temp_dir().join(format!("jstream-portal-tokens-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let tokens = RestoreTokens {
                dir: Some(dir.clone()),
                ..RestoreTokens::default()
            };
            assert_eq!(tokens.get(PortalPick::Display), None);
            tokens.put(PortalPick::Display, "3f2a-uuid");
            tokens.put(PortalPick::Window, "bad token\n");
            assert_eq!(
                tokens.get(PortalPick::Window),
                None,
                "implausible tokens are refused"
            );
            // A new store on the same folder reads what the first wrote.
            let again = RestoreTokens {
                dir: Some(dir.clone()),
                ..RestoreTokens::default()
            };
            assert_eq!(again.get(PortalPick::Display).as_deref(), Some("3f2a-uuid"));
            let mode = std::fs::metadata(dir.join("screencast-display.token"))
                .expect("written")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "only the user can read a token");
            // A replacement token wins, in memory and on disk.
            again.put(PortalPick::Display, "next");
            assert_eq!(again.get(PortalPick::Display).as_deref(), Some("next"));

            // A token file others can read is refused, and the next write makes it
            // private again, though it already existed.
            let file = dir.join("screencast-window.token");
            std::fs::write(&file, "leaked").expect("written");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");
            let fresh = RestoreTokens {
                dir: Some(dir.clone()),
                ..RestoreTokens::default()
            };
            assert_eq!(
                fresh.get(PortalPick::Window),
                None,
                "a readable file is refused"
            );
            fresh.put(PortalPick::Window, "private");
            let mode = std::fs::metadata(&file)
                .expect("there")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "an existing file is made private");
            let reread = RestoreTokens {
                dir: Some(dir.clone()),
                ..RestoreTokens::default()
            };
            assert_eq!(reread.get(PortalPick::Window).as_deref(), Some("private"));
            assert!(!dir.join("screencast-window.token.tmp").exists());

            // A symlink is never followed.
            let target = dir.join("elsewhere");
            std::fs::write(&target, "planted").expect("written");
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))
                .expect("chmod");
            let link = dir.join("screencast-display.token");
            std::fs::remove_file(&link).expect("removed");
            std::os::unix::fs::symlink(&target, &link).expect("linked");
            let linked = RestoreTokens {
                dir: Some(dir.clone()),
                ..RestoreTokens::default()
            };
            assert_eq!(
                linked.get(PortalPick::Display),
                None,
                "a symlink is refused"
            );
            let _ = std::fs::remove_dir_all(&dir);

            let memory = RestoreTokens::default();
            memory.put(PortalPick::Window, "w");
            assert_eq!(memory.get(PortalPick::Window).as_deref(), Some("w"));
            assert_eq!(memory.get(PortalPick::Display), None);
            assert!(!plausible(""));
            assert!(!plausible(&"x".repeat(300)));
        }
    }
}

/// Platform tests against a real desktop (R-TEST-004): behind `hw-tests` and
/// `#[ignore]`d, because CI has no session bus, portal or PipeWire daemon. Run them in a
/// GNOME or KDE Wayland session and pick a screen when asked:
/// `cargo test -p jstream-platform-linux --features hw-tests -- --ignored portal`.
#[cfg(all(test, target_os = "linux", feature = "hw-tests"))]
mod hw_tests {
    use std::time::{Duration, Instant};

    use jstream_platform::{
        Capture, CaptureTarget, DeviceId, GpuInterop, MediaClock, VideoSurface, media::PixelFormat,
    };

    use crate::{LinuxGpu, MonotonicClock, PortalCapture, Session};

    /// On a GPU with DMA-BUF import: the capture on the context delivers textures on
    /// its device, recent and of the picked screen's size.
    #[test]
    #[ignore = "needs a desktop session with the ScreenCast portal, PipeWire and a Vulkan GPU with DMA-BUF import"]
    fn portal_capture_on_the_gpu_delivers_textures() {
        let context = LinuxGpu::new().open_device().expect("a Vulkan GPU");
        let capture = PortalCapture::new(Session::from_env())
            .on_context(&context)
            .expect("our context")
            .expect("the GPU imports DMA-BUFs");
        let clock = MonotonicClock::new().expect("clock");
        let target = CaptureTarget::Display(DeviceId::new("portal:display"));
        let mut stream = capture.start(&target).expect("pick a screen in the dialog");
        let deadline = Instant::now() + Duration::from_secs(10);
        let frame = loop {
            if let Some(f) = stream.latest().expect("capturing") {
                break f;
            }
            assert!(Instant::now() < deadline, "no frame within 10 s");
            std::thread::sleep(Duration::from_millis(16));
        };
        let VideoSurface::Gpu(frame) = frame else {
            panic!("expected a GPU frame: the DMA-BUF path fell back to shared memory");
        };
        assert_eq!(frame.info().format, PixelFormat::Rgba8);
        assert_eq!(frame.texture().width(), frame.info().size.width());
        frame.stage(context.queue()).expect("the context's queue");
        let age = clock.now().ticks() - frame.info().pts.ticks();
        assert!((0..10_000_000).contains(&age), "pts {age} ticks old");
    }

    #[test]
    #[ignore = "needs a desktop session with the ScreenCast portal and PipeWire"]
    fn portal_capture_delivers_recent_frames_and_keeps_the_grant() {
        let dir = std::env::temp_dir().join("jstream-hw-portal-tokens");
        let capture = PortalCapture::with_token_dir(Session::from_env(), dir.clone());
        let target = CaptureTarget::Display(DeviceId::new("portal:display"));
        let clock = MonotonicClock::new().expect("clock");
        let mut stream = capture.start(&target).expect("pick a screen in the dialog");
        let deadline = Instant::now() + Duration::from_secs(10);
        let frame = loop {
            if let Some(f) = stream.latest().expect("capturing") {
                break f;
            }
            assert!(Instant::now() < deadline, "no frame within 10 s");
            std::thread::sleep(Duration::from_millis(16));
        };
        let VideoSurface::Cpu(frame) = frame else {
            panic!("expected a CPU frame");
        };
        assert!(frame.info().size.width() > 0);
        let age = clock.now().ticks() - frame.info().pts.ticks();
        assert!((0..10_000_000).contains(&age), "pts {age} ticks old");
        assert!(stream.stats().frames_captured >= 1);
        drop(stream);
        // The grant was kept: the second start reuses it without a dialog.
        assert!(
            dir.join("screencast-display.token").exists(),
            "a restore token was stored"
        );
        let mut again = capture.start(&target).expect("restored without a dialog");
        let _ = again.latest();
    }
}

#[cfg(test)]
mod tests {
    use super::{CursorModes, PersistMode, PortalPick, SourceTypes, handle_token, request_path};

    #[test]
    fn option_values_match_the_portal() {
        assert_eq!(SourceTypes::MONITOR.0 | SourceTypes::WINDOW.0, 3);
        assert!(SourceTypes(7).contains(SourceTypes::VIRTUAL));
        assert!(!SourceTypes(1).contains(SourceTypes::WINDOW));
        assert_eq!(PersistMode::UntilRevoked as u32, 2);
        assert_eq!(PortalPick::Window.source_types(), SourceTypes::WINDOW);
    }

    #[test]
    fn cursor_mode_prefers_metadata_then_embedded() {
        assert_eq!(
            CursorModes::preferred(CursorModes(7)),
            CursorModes::METADATA
        );
        assert_eq!(
            CursorModes::preferred(CursorModes(3)),
            CursorModes::EMBEDDED
        );
        assert_eq!(CursorModes::preferred(CursorModes(1)), CursorModes::HIDDEN);
        assert_eq!(CursorModes::preferred(CursorModes(0)), CursorModes::HIDDEN);
    }

    #[test]
    fn request_paths_follow_the_portal_convention() {
        let token = handle_token(7);
        assert_eq!(token, "jstream_7");
        assert_eq!(
            request_path(":1.42", &token).as_deref(),
            Some("/org/freedesktop/portal/desktop/request/1_42/jstream_7")
        );
        assert_eq!(request_path("org.example", &token), None);
        assert_eq!(request_path(":1.42", "bad-token"), None);
        assert_eq!(request_path(":1.42", "a.b"), None);
        assert_eq!(request_path(":", &token), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn capture_lists_one_target_per_kind_and_refuses_others() {
        use jstream_platform::{Capture, CaptureTarget, DeviceId, PlatformError};

        use crate::{PortalCapture, Session, SessionKind};

        let capture = PortalCapture::new(Session {
            kind: SessionKind::Wayland,
            desktops: vec!["GNOME".into()],
        });
        let targets = capture.targets().expect("lists without D-Bus");
        assert_eq!(targets.len(), 2);
        assert_eq!(
            targets[0].target,
            CaptureTarget::Display(DeviceId::new("portal:display"))
        );
        // Starting a listed target opens the portal, which needs a desktop (hw_tests).
        assert!(matches!(
            capture.start(&CaptureTarget::Display(DeviceId::new("HDMI-1"))),
            Err(PlatformError::NotFound {
                kind: "display",
                ..
            })
        ));

        let headless = PortalCapture::new(Session {
            kind: SessionKind::Headless,
            desktops: Vec::new(),
        });
        assert!(headless.targets().expect("lists").is_empty());
        assert!(matches!(
            headless.start(&targets[1].target),
            Err(PlatformError::Unsupported { .. })
        ));
    }
}
