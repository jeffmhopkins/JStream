//! In-memory transports for hermetic tests (docs/RULES.md#R-TEST-004).

use std::io::{self, Read, Write};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Replays fixed input and records everything written.
pub(crate) struct Scripted {
    input: io::Cursor<Vec<u8>>,
    output: Vec<u8>,
}

impl Scripted {
    pub(crate) fn new(input: Vec<u8>) -> Self {
        Self {
            input: io::Cursor::new(input),
            output: Vec::new(),
        }
    }

    pub(crate) fn written(&self) -> &[u8] {
        &self.output
    }

    pub(crate) fn fully_read(&self) -> bool {
        self.input.position() as usize == self.input.get_ref().len()
    }
}

impl Read for Scripted {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.input.read(buf)
    }
}

impl Write for Scripted {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One end of an in-memory, blocking, full-duplex byte pipe.
///
/// Reads block until the other end writes or is dropped (then they return end of
/// stream). With `nonblocking`, a read with nothing pending fails with `WouldBlock`,
/// like a socket with a read timeout.
pub(crate) struct Duplex {
    tx: Sender<Vec<u8>>,
    rx: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    pub(crate) nonblocking: bool,
}

/// Two connected [`Duplex`] ends.
pub(crate) fn duplex() -> (Duplex, Duplex) {
    let (a_tx, b_rx) = channel();
    let (b_tx, a_rx) = channel();
    let end = |tx, rx| Duplex {
        tx,
        rx,
        pending: Vec::new(),
        nonblocking: false,
    };
    (end(a_tx, a_rx), end(b_tx, b_rx))
}

impl Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pending.is_empty() {
            let next = if self.nonblocking {
                match self.rx.try_recv() {
                    Ok(v) => Some(v),
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        return Err(io::ErrorKind::WouldBlock.into());
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
                }
            } else {
                self.rx.recv().ok()
            };
            match next {
                Some(v) => self.pending = v,
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

impl Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tx
            .send(buf.to_vec())
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
