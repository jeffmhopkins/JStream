//! Runs one `Control` future to completion on the calling thread.
//!
//! Plugin code is synchronous: a host function can't return until its call is answered.
//! Rather than wasmtime's async support (fibers, and an executor of the host's choosing),
//! the host parks the thread running the plugin until the future wakes it. So a plugin
//! must be invoked on a thread that may block, never on an async executor's worker.

use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

/// Wakes the parked thread.
struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Polls `future` until it is ready, parking the thread between polls.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        // A spurious unpark only costs one more poll.
        thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::block_on;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn a_ready_future_answers_at_once() {
        assert_eq!(block_on(async { 7 }), 7);
    }

    #[test]
    fn a_future_woken_from_another_thread_completes() {
        // A future that is pending until another thread sends, waking it.
        struct Later(mpsc::Receiver<u32>, Option<mpsc::Sender<std::task::Waker>>);
        impl std::future::Future for Later {
            type Output = u32;
            fn poll(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<u32> {
                if let Ok(v) = self.0.try_recv() {
                    return std::task::Poll::Ready(v);
                }
                if let Some(tx) = self.1.take() {
                    let _ = tx.send(cx.waker().clone());
                }
                std::task::Poll::Pending
            }
        }
        let (value_tx, value_rx) = mpsc::channel();
        let (waker_tx, waker_rx) = mpsc::channel::<std::task::Waker>();
        let sender = thread::spawn(move || {
            let waker = waker_rx.recv().expect("the future registers its waker");
            thread::sleep(Duration::from_millis(10));
            value_tx.send(42).expect("the future is still waiting");
            waker.wake();
        });
        assert_eq!(block_on(Later(value_rx, Some(waker_tx))), 42);
        sender.join().expect("the sender thread finishes");
    }
}
