//! Driving `Control`'s futures and event stream to completion on the calling thread.
//!
//! `Control` returns boxed futures that need no particular executor (see
//! crates/jstream-control/README.md#design). A CLI does one thing at a time, so a
//! thread-parking `block_on` is enough: no async runtime. The mock's futures are always
//! ready, so they complete on the first poll; a transport that completes its futures from
//! another thread (a socket reader) wakes this one through the waker.

use jstream_control::EventStream;
use jstream_control::protocol::Event;
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

/// Wakes the thread blocked in [`block_on`].
struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Polls `future` on this thread until it completes, parking between polls.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        // A wake that raced ahead of this park leaves the token set, so park returns at
        // once; a spurious return just polls again.
        thread::park();
    }
}

/// The next event, waiting for it if needed, or `None` once the stream has ended.
pub fn next_event(events: &mut EventStream) -> Option<Event> {
    block_on(poll_fn(|cx| events.as_mut().poll_next(cx)))
}

#[cfg(test)]
mod tests {
    use super::block_on;
    use std::future::poll_fn;
    use std::sync::mpsc;
    use std::task::Poll;
    use std::thread;

    #[test]
    fn ready_futures_complete_at_once() {
        assert_eq!(block_on(async { 7 }), 7);
    }

    #[test]
    fn a_wake_from_another_thread_resumes_the_poll() {
        let (tx, rx) = mpsc::channel::<u32>();
        let mut waker_sent = false;
        let mut helper = None;
        let value = block_on(poll_fn(|cx| {
            if let Ok(v) = rx.try_recv() {
                return Poll::Ready(v);
            }
            if !waker_sent {
                waker_sent = true;
                let waker = cx.waker().clone();
                let tx = tx.clone();
                helper = Some(thread::spawn(move || {
                    tx.send(42).expect("receiver alive");
                    waker.wake();
                }));
            }
            Poll::Pending
        }));
        assert_eq!(value, 42);
        if let Some(helper) = helper {
            helper.join().expect("helper thread");
        }
    }
}
