//! The stdio transport: newline-delimited JSON-RPC over a byte stream.
//!
//! It runs on the calling thread and needs no async runtime: each message's future is
//! driven by a minimal `block_on` that parks the thread until the [`Control`] future
//! wakes it. That works for any `Control` whose futures are woken by their own machinery
//! (channels, a reactor on another thread), which is what the trait asks for.
//!
//! [`Control`]: jstream_control::Control

use crate::error::McpError;
use crate::jsonrpc::{self, RpcError};
use crate::server::Server;
use std::future::Future;
use std::io::{BufRead, Write};
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

/// Serves MCP over a byte stream until `input` ends: one JSON-RPC message per line in,
/// one reply per line out (MCP stdio transport).
///
/// Blank lines are skipped. A line that isn't UTF-8 is answered with a parse error. Each
/// reply is flushed as soon as it is written. Messages are answered one at a time, in
/// order. Returns `Ok(())` at end of input, the transport's signal to shut down.
///
/// A host passes `std::io::stdin().lock()` and `std::io::stdout()`; nothing else may write
/// to that stdout, because every line on it must be an MCP message.
pub fn serve_stdio(
    server: &Server,
    mut input: impl BufRead,
    mut output: impl Write,
) -> Result<(), McpError> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        let reply = match std::str::from_utf8(&line) {
            Ok(text) if text.trim().is_empty() => continue,
            Ok(text) => block_on(server.handle(text)),
            Err(e) => Some(
                jsonrpc::error_reply(
                    None,
                    &RpcError::new(jsonrpc::PARSE_ERROR, format!("parse error: {e}")),
                )
                .to_string(),
            ),
        };
        if let Some(reply) = reply {
            output.write_all(reply.as_bytes())?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
}

/// Wakes the thread that is blocked in [`block_on`].
struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Runs `future` to completion on this thread, parking it while the future is pending.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => return output,
            // A spurious unpark just polls again.
            Poll::Pending => thread::park(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::block_on;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};
    use std::thread;
    use std::time::Duration;

    /// Pending until another thread sets the value and wakes it.
    struct Slot(Arc<Mutex<(Option<u32>, Option<Waker>)>>);

    impl Future for Slot {
        type Output = u32;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u32> {
            let mut slot = self.0.lock().expect("not poisoned");
            match slot.0 {
                Some(v) => Poll::Ready(v),
                None => {
                    slot.1 = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        }
    }

    #[test]
    fn block_on_waits_for_a_wake_from_another_thread() {
        let shared: Arc<Mutex<(Option<u32>, Option<Waker>)>> = Arc::new(Mutex::new((None, None)));
        let setter = Arc::clone(&shared);
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            let waker = {
                let mut slot = setter.lock().expect("not poisoned");
                slot.0 = Some(7);
                slot.1.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        });
        assert_eq!(block_on(Slot(shared)), 7);
        handle.join().expect("setter thread");
    }
}
