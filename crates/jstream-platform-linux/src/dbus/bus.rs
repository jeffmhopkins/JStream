//! The session bus connection: a Unix socket, `EXTERNAL` authentication with Unix file
//! descriptor passing, and blocking calls with timeouts.
//!
//! One thread uses a connection at a time. Signals that arrive while a call waits for
//! its reply are queued for [`Bus::wait_signal`].

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use jstream_platform::PlatformError;

use super::{
    Kind, MAX_MESSAGE, Message, SocketAddr, Value, auth_external, is_reply, parse_address,
};

/// The bus daemon's own name, the sender of its replies and signals.
pub(crate) const BUS: &str = "org.freedesktop.DBus";

/// File descriptors accepted per `recvmsg`. The portal sends one.
const MAX_FDS: usize = 8;

/// A received message and the file descriptors it carried.
#[derive(Debug)]
pub(crate) struct Received {
    pub(crate) msg: Message,
    pub(crate) fds: Vec<OwnedFd>,
}

/// A connection to the session bus.
#[derive(Debug)]
pub(crate) struct Bus {
    sock: UnixStream,
    /// Bytes received and not yet parsed into a message.
    buf: Vec<u8>,
    /// File descriptors received and not yet handed to a message, in order.
    fds: VecDeque<OwnedFd>,
    /// Messages received while waiting for something else.
    queue: VecDeque<Received>,
    serial: u32,
    unique_name: String,
}

fn io_err(call: &'static str, e: &io::Error) -> PlatformError {
    PlatformError::Os {
        call,
        code: e.raw_os_error().and_then(|c| u32::try_from(c).ok()),
        message: e.to_string(),
    }
}

fn timed_out(what: &'static str) -> PlatformError {
    PlatformError::Os {
        call: what,
        code: None,
        message: "timed out waiting for the reply".into(),
    }
}

impl Bus {
    /// Connects to the session bus named by `DBUS_SESSION_BUS_ADDRESS`, else
    /// `$XDG_RUNTIME_DIR/bus`, authenticates and says `Hello`.
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when there is no session bus, and
    /// [`PlatformError::Os`] when connecting or authenticating fails.
    pub(crate) fn session() -> Result<Self, PlatformError> {
        let addr = std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .ok()
            .and_then(|a| parse_address(&a))
            .or_else(|| {
                let dir = std::env::var("XDG_RUNTIME_DIR").ok()?;
                Some(SocketAddr::Path(format!("{dir}/bus")))
            })
            .ok_or_else(|| PlatformError::MissingComponent {
                component: "D-Bus session bus",
                remedy: "run JStream in a desktop session (DBUS_SESSION_BUS_ADDRESS and \
                         XDG_RUNTIME_DIR are both unset)"
                    .into(),
            })?;
        let sock = match &addr {
            SocketAddr::Path(p) => UnixStream::connect(p),
            SocketAddr::Abstract(name) => std::os::unix::net::SocketAddr::from_abstract_name(name)
                .and_then(|a| UnixStream::connect_addr(&a)),
        }
        .map_err(|e| io_err("connecting to the D-Bus session bus", &e))?;
        let mut bus = Self {
            sock,
            buf: Vec::new(),
            fds: VecDeque::new(),
            queue: VecDeque::new(),
            serial: 0,
            unique_name: String::new(),
        };
        bus.authenticate()?;
        let hello = bus.call(
            Message::call(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "Hello",
                Vec::new(),
            ),
            Some(BUS),
            "org.freedesktop.DBus.Hello",
            Duration::from_secs(5),
        )?;
        bus.unique_name = hello
            .msg
            .body
            .first()
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| PlatformError::Os {
                call: "org.freedesktop.DBus.Hello",
                code: None,
                message: "the reply has no unique name".into(),
            })?;
        Ok(bus)
    }

    /// This connection's unique name, such as `:1.42`.
    pub(crate) fn unique_name(&self) -> &str {
        &self.unique_name
    }

    fn authenticate(&mut self) -> Result<(), PlatformError> {
        let uid = std::fs::metadata("/proc/self")
            .map_err(|e| io_err("reading the user ID", &e))?
            .uid();
        let auth = |e: &io::Error| io_err("D-Bus authentication", e);
        self.sock
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| auth(&e))?;
        self.sock
            .write_all(&auth_external(uid))
            .map_err(|e| auth(&e))?;
        let ok = self.line()?;
        if !ok.starts_with("OK ") {
            return Err(PlatformError::Os {
                call: "D-Bus authentication",
                code: None,
                message: format!("the bus refused EXTERNAL authentication: {ok:?}"),
            });
        }
        self.sock
            .write_all(b"NEGOTIATE_UNIX_FD\r\n")
            .map_err(|e| auth(&e))?;
        let agree = self.line()?;
        if agree != "AGREE_UNIX_FD" {
            return Err(PlatformError::Unsupported {
                what: format!("a D-Bus session bus without file descriptor passing ({agree:?})"),
            });
        }
        self.sock.write_all(b"BEGIN\r\n").map_err(|e| auth(&e))
    }

    /// Reads one `\r\n`-terminated authentication line, a byte at a time so nothing
    /// after it is consumed.
    fn line(&mut self) -> Result<String, PlatformError> {
        let mut line = Vec::new();
        let mut byte = [0u8];
        while !line.ends_with(b"\r\n") {
            if line.len() > 512 {
                return Err(PlatformError::Os {
                    call: "D-Bus authentication",
                    code: None,
                    message: "line too long".into(),
                });
            }
            match self.sock.read(&mut byte) {
                Ok(0) => {
                    return Err(PlatformError::Os {
                        call: "D-Bus authentication",
                        code: None,
                        message: "the bus closed the connection".into(),
                    });
                }
                Ok(_) => line.push(byte[0]),
                Err(e) => return Err(io_err("D-Bus authentication", &e)),
            }
        }
        line.truncate(line.len() - 2);
        Ok(String::from_utf8_lossy(&line).into_owned())
    }

    /// Sends `msg` and returns its serial.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the socket refuses it.
    pub(crate) fn send(&mut self, msg: &Message) -> Result<u32, PlatformError> {
        self.serial = self.serial.wrapping_add(1).max(1);
        let mut bytes = msg.encode(self.serial)?;
        let written = self
            .sock
            .write_all(&bytes)
            .map_err(|e| io_err("writing to the D-Bus session bus", &e));
        // A message can carry a secret (the Secret Service's `CreateItem`), so the
        // encoded copy is zeroed before it is freed.
        wipe(&mut bytes);
        written?;
        Ok(self.serial)
    }

    /// Calls a method and waits up to `timeout` for its reply, which must come from
    /// `from` (the callee's unique name, or `org.freedesktop.DBus` for the bus itself)
    /// when it is given: a reply from any other connection is dropped. An error reply
    /// is an error named `what`; a missing service is a missing component.
    ///
    /// # Errors
    /// As described, and [`PlatformError::Os`] on a timeout or a broken connection.
    pub(crate) fn call(
        &mut self,
        msg: Message,
        from: Option<&str>,
        what: &'static str,
        timeout: Duration,
    ) -> Result<Received, PlatformError> {
        let reply = self.call_reply(&msg, from, what, timeout)?;
        check_reply(reply, what)
    }

    /// As [`call`](Self::call), but an error reply is returned as it came, for a caller
    /// that tells its error names apart itself (the Secret Service store).
    ///
    /// # Errors
    /// [`PlatformError::Os`] on a timeout or a broken connection.
    pub(crate) fn call_reply(
        &mut self,
        msg: &Message,
        from: Option<&str>,
        what: &'static str,
        timeout: Duration,
    ) -> Result<Received, PlatformError> {
        let serial = self.send(msg)?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(i) = self
                .queue
                .iter()
                .position(|r| is_reply(&r.msg, serial, from))
            {
                return self.queue.remove(i).ok_or_else(|| timed_out(what));
            }
            let got = self.receive(deadline)?.ok_or_else(|| timed_out(what))?;
            if got.msg.reply_serial == Some(serial) && !is_reply(&got.msg, serial, from) {
                continue; // Claims to answer this call, but not from the callee.
            }
            if self.queue.len() >= 64 {
                self.queue.pop_front();
            }
            self.queue.push_back(got);
        }
    }

    /// Waits until `deadline` for a signal `accept` takes, removing it from the queue.
    /// `Ok(None)` on the deadline.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the connection breaks.
    pub(crate) fn wait_signal(
        &mut self,
        deadline: Instant,
        mut accept: impl FnMut(&Message) -> bool,
    ) -> Result<Option<Message>, PlatformError> {
        loop {
            if let Some(i) = self
                .queue
                .iter()
                .position(|r| r.msg.kind == Kind::Signal && accept(&r.msg))
            {
                return Ok(self.queue.remove(i).map(|r| r.msg));
            }
            // Drop what nobody will ask for: replies to calls that timed out, and
            // signals the caller didn't take. Keeps the queue bounded.
            self.queue.retain(|r| r.msg.kind == Kind::Signal);
            if self.queue.len() > 64 {
                self.queue.pop_front();
            }
            match self.receive(deadline)? {
                Some(got) => self.queue.push_back(got),
                None => return Ok(None),
            }
        }
    }

    /// Receives the next whole message, or `None` once `deadline` passes.
    fn receive(&mut self, deadline: Instant) -> Result<Option<Received>, PlatformError> {
        loop {
            if let Some(len) = Message::frame_len(&self.buf)?
                && self.buf.len() >= len
            {
                let rest = self.buf.split_off(len);
                let mut bytes = std::mem::replace(&mut self.buf, rest);
                let msg = Message::decode(&bytes);
                // A reply can carry a secret (the Secret Service's `GetSecret`).
                wipe(&mut bytes);
                let msg = msg?;
                let n = usize::try_from(msg.unix_fds).unwrap_or(usize::MAX);
                if n > self.fds.len() {
                    return Err(PlatformError::Os {
                        call: "D-Bus",
                        code: None,
                        message: "a message announced file descriptors that didn't arrive".into(),
                    });
                }
                let fds = self.fds.drain(..n).collect();
                return Ok(Some(Received { msg, fds }));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let wait = (deadline - now).max(Duration::from_millis(1));
            self.sock
                .set_read_timeout(Some(wait))
                .map_err(|e| io_err("reading from the D-Bus session bus", &e))?;
            match recv_with_fds(&self.sock) {
                Ok((bytes, fds)) if bytes.is_empty() => {
                    drop(fds);
                    return Err(PlatformError::Os {
                        call: "reading from the D-Bus session bus",
                        code: None,
                        message: "the bus closed the connection".into(),
                    });
                }
                Ok((mut bytes, fds)) => {
                    self.buf.extend_from_slice(&bytes);
                    wipe(&mut bytes);
                    self.fds.extend(fds);
                    if self.fds.len() > 4 * MAX_FDS {
                        // Descriptors no message claims: drop the oldest.
                        self.fds.drain(..MAX_FDS);
                    }
                    if self.buf.len() > MAX_MESSAGE + 16 {
                        return Err(PlatformError::Os {
                            call: "D-Bus",
                            code: None,
                            message: "a message is too long".into(),
                        });
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(io_err("reading from the D-Bus session bus", &e)),
            }
        }
    }
}

/// Zeroes a buffer, its spare capacity included, before it is freed. Best effort, as
/// for `Secret`: `black_box` keeps the writes from being dropped as dead stores, and
/// copies left behind when a buffer grew are not reached.
fn wipe(bytes: &mut Vec<u8>) {
    let capacity = bytes.capacity();
    bytes.clear();
    // Within the capacity, so this writes zeros over the whole allocation and never
    // reallocates.
    bytes.resize(capacity, 0);
    std::hint::black_box(&*bytes);
}

/// A method's reply, or its error as a [`PlatformError`].
fn check_reply(reply: Received, what: &'static str) -> Result<Received, PlatformError> {
    if reply.msg.kind != Kind::Error {
        return Ok(reply);
    }
    let name = reply.msg.error_name.clone().unwrap_or_default();
    let text = reply
        .msg
        .body
        .first()
        .and_then(Value::as_str)
        .unwrap_or_default();
    if name == "org.freedesktop.DBus.Error.ServiceUnknown"
        || name == "org.freedesktop.DBus.Error.NameHasNoOwner"
        || name == "org.freedesktop.DBus.Error.UnknownMethod"
    {
        return Err(PlatformError::MissingComponent {
            component: "xdg-desktop-portal",
            remedy: format!(
                "install xdg-desktop-portal and the desktop's backend (xdg-desktop-portal-gnome, \
                 -kde or -wlr) ({what}: {name})"
            ),
        });
    }
    Err(PlatformError::Os {
        call: what,
        code: None,
        message: format!("{name}: {text}"),
    })
}

/// One `recvmsg` on `sock`: the bytes read (empty at end of stream) and the file
/// descriptors that came with them, owned.
fn recv_with_fds(sock: &UnixStream) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut data = vec![0u8; 64 * 1024];
    // `CMSG_SPACE` is a `const fn` in libc but `unsafe`; the room for MAX_FDS ints,
    // computed as glibc does it: the header, then the payload, each aligned to `size_t`.
    let align = |n: usize| n.next_multiple_of(std::mem::size_of::<usize>());
    let space = align(std::mem::size_of::<libc::cmsghdr>())
        + align(MAX_FDS * std::mem::size_of::<libc::c_int>());
    // u64 elements so the buffer is aligned for `cmsghdr`.
    let mut control = vec![0u64; space.div_ceil(8)];
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr().cast(),
        iov_len: data.len(),
    };
    // SAFETY: `msghdr` is plain data; all-zero is a valid (empty) value.
    let mut hdr: libc::msghdr = unsafe { std::mem::zeroed() };
    hdr.msg_iov = &raw mut iov;
    hdr.msg_iovlen = 1;
    hdr.msg_control = control.as_mut_ptr().cast();
    hdr.msg_controllen = control.len() * 8;
    // SAFETY: `ssize_t recvmsg(int sockfd, struct msghdr *msg, int flags)`. The socket
    // is open for the call (borrowed), `hdr` points at one iovec over `data` and at
    // `control`, both alive and of the lengths given. MSG_CMSG_CLOEXEC marks received
    // descriptors close-on-exec.
    let n = unsafe { libc::recvmsg(sock.as_raw_fd(), &raw mut hdr, libc::MSG_CMSG_CLOEXEC) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut fds = Vec::new();
    // SAFETY: `struct cmsghdr *CMSG_FIRSTHDR(struct msghdr *msgh)`: reads
    // `msg_control`/`msg_controllen`, which recvmsg filled within `control`.
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&raw const hdr) };
    while !cmsg.is_null() {
        // SAFETY: non-null, so it points at a complete `cmsghdr` inside `control`
        // (CMSG_FIRSTHDR and CMSG_NXTHDR check the bounds).
        let c = unsafe { &*cmsg };
        if c.cmsg_level == libc::SOL_SOCKET && c.cmsg_type == libc::SCM_RIGHTS {
            // SAFETY: `unsigned char *CMSG_DATA(struct cmsghdr *cmsg)`, for the header
            // above; `unsigned int CMSG_LEN(unsigned int length)` is arithmetic.
            let (payload, header) = unsafe { (libc::CMSG_DATA(cmsg), libc::CMSG_LEN(0) as usize) };
            let count =
                (c.cmsg_len as usize).saturating_sub(header) / std::mem::size_of::<libc::c_int>();
            for i in 0..count {
                // SAFETY: the kernel wrote `count` ints after the header; the payload
                // may be unaligned, so it is read unaligned.
                let fd = unsafe {
                    payload
                        .add(i * std::mem::size_of::<libc::c_int>())
                        .cast::<libc::c_int>()
                        .read_unaligned()
                };
                if fd >= 0 {
                    // SAFETY: SCM_RIGHTS installed `fd` in this process for us; nothing
                    // else owns it.
                    fds.push(unsafe { OwnedFd::from_raw_fd(fd) });
                }
            }
        }
        // SAFETY: `struct cmsghdr *CMSG_NXTHDR(struct msghdr *msgh, struct cmsghdr
        // *cmsg)`, with `cmsg` from this same `hdr`; it returns null past the end.
        cmsg = unsafe { libc::CMSG_NXTHDR(&raw const hdr, cmsg) };
    }
    data.truncate(usize::try_from(n).unwrap_or(0));
    Ok((data, fds))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    use super::recv_with_fds;

    /// Passes a descriptor over a socket pair, as the portal passes the PipeWire
    /// remote, and checks it arrives open and usable. No bus needed.
    #[test]
    fn descriptors_arrive_owned() {
        let (a, b) = UnixStream::pair().expect("a socket pair");
        let (mut pipe_r, mut pipe_w) = UnixStream::pair().expect("another pair");
        let fd = pipe_w.as_raw_fd();
        let bytes = b"hello";
        let mut iov = libc::iovec {
            iov_base: bytes.as_ptr().cast_mut().cast(),
            iov_len: bytes.len(),
        };
        let mut control = [0u64; 4];
        // SAFETY: plain data, zero is valid.
        let mut hdr: libc::msghdr = unsafe { std::mem::zeroed() };
        hdr.msg_iov = &raw mut iov;
        hdr.msg_iovlen = 1;
        hdr.msg_control = control.as_mut_ptr().cast();
        // SAFETY: `CMSG_SPACE(unsigned int)` is arithmetic.
        hdr.msg_controllen = unsafe { libc::CMSG_SPACE(4) } as usize;
        // SAFETY: as in `recv_with_fds`; `control` has room for one fd.
        unsafe {
            let c = libc::CMSG_FIRSTHDR(&raw const hdr);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(4) as usize;
            libc::CMSG_DATA(c).cast::<libc::c_int>().write_unaligned(fd);
        }
        // SAFETY: `ssize_t sendmsg(int sockfd, const struct msghdr *msg, int flags)`
        // with `hdr` describing live buffers.
        let sent = unsafe { libc::sendmsg(a.as_raw_fd(), &raw const hdr, 0) };
        assert_eq!(sent, 5);
        let (data, fds) = recv_with_fds(&b).expect("received");
        assert_eq!(data, b"hello");
        assert_eq!(fds.len(), 1);
        pipe_w.write_all(b"x").expect("the original end works");
        let mut received = UnixStream::from(fds.into_iter().next().expect("one fd"));
        received.write_all(b"y").expect("the passed end works");
        let mut got = [0u8; 2];
        pipe_r.read_exact(&mut got).expect("both bytes");
        assert_eq!(&got, b"xy");
    }
}

/// Platform tests against a real session bus (R-TEST-004): behind `hw-tests` and
/// `#[ignore]`d, because CI has no session bus. Any desktop session has one, and
/// `dbus-run-session` starts one: `dbus-run-session -- cargo test -p
/// jstream-platform-linux --features hw-tests -- --ignored bus`.
#[cfg(all(test, feature = "hw-tests"))]
mod hw_tests {
    use std::time::Duration;

    use super::{BUS, Bus};
    use crate::dbus::{Message, Value, add_match, match_rule};

    fn bus_call(member: &str) -> Message {
        Message::call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            member,
            Vec::new(),
        )
    }

    #[test]
    #[ignore = "needs a D-Bus session bus"]
    fn the_session_bus_answers() {
        let mut bus = Bus::session().expect("a session bus");
        assert!(bus.unique_name().starts_with(':'), "{}", bus.unique_name());
        let t = Duration::from_secs(5);
        let id = bus
            .call(bus_call("GetId"), Some(BUS), "GetId", t)
            .expect("an ID");
        assert!(matches!(id.msg.body.as_slice(), [Value::Str(s)] if s.len() == 32));
        let names = bus
            .call(bus_call("ListNames"), Some(BUS), "ListNames", t)
            .expect("names");
        let [Value::Array(elem, names)] = names.msg.body.as_slice() else {
            panic!("expected as, got {:?}", names.msg.body);
        };
        assert_eq!(elem, "s");
        assert!(names.contains(&Value::Str(bus.unique_name().to_owned())));
        let rule = match_rule(BUS, "org.freedesktop.portal.Request", "Response");
        bus.call(add_match(&rule), Some(BUS), "AddMatch", t)
            .expect("a match");
        // A reply must come from the callee: the bus's reply to GetId isn't from a
        // made-up unique name, so the call times out rather than taking it.
        let quick = Duration::from_millis(300);
        assert!(
            bus.call(bus_call("GetId"), Some(":1.9999"), "GetId", quick)
                .is_err()
        );
        let missing = Message::call(
            "org.jstream.NoSuchService",
            "/",
            "org.jstream.X",
            "Y",
            Vec::new(),
        );
        assert!(
            bus.call(missing, None, "Y", t).is_err(),
            "an error reply is an error"
        );
    }
}
