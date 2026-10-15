//! Preview frames on a connection: the session's binary channel (`Control::binary`)
//! on one side, WebSocket binary messages on the other (ADR-0002).
//!
//! The connection opens the session's binary channel once the handshake succeeds, and
//! queues each frame it yields in [`Frames`] until the writer sends it. The queue holds
//! [`FRAME_BUFFER`] frames. Like the event outbox, it never makes the engine wait: a
//! frame that finds it full is dropped, and so is the rest of its stream until a
//! keyframe finds room ([`KeyframeGate`]), so what the client gets always decodes.
//! The engine sends a keyframe at least every 2 s, which bounds how long that takes.
//! JSON messages go first: a frame is sent only when no reply or event is waiting.

use futures_util::StreamExt;
use jstream_control::BinaryStream;
use jstream_protocol::{BinaryFrame, KeyframeGate};
use std::collections::VecDeque;

/// How many binary frames a connection holds for a client that reads slowly. At a
/// preview's 15 to 30 frames a second that is about a quarter to half a second.
pub(crate) const FRAME_BUFFER: usize = 8;

/// The binary frames waiting to be sent, oldest first.
#[derive(Default)]
pub(crate) struct Frames {
    queue: VecDeque<BinaryFrame>,
    gate: KeyframeGate,
}

impl Frames {
    /// Queues `frame`, or drops it when the queue is full or its stream is waiting for
    /// a keyframe.
    pub(crate) fn push(&mut self, frame: BinaryFrame) {
        let channel = frame.header.channel;
        let was_waiting = self.gate.is_waiting(channel);
        let room = self.queue.len() < FRAME_BUFFER;
        if self.gate.admit(&frame.header, room) {
            self.queue.push_back(frame);
        } else if !was_waiting {
            tracing::debug!(
                channel,
                "client reads too slowly: preview frames dropped until a keyframe"
            );
        }
    }

    /// The next frame to send, as its wire bytes.
    pub(crate) fn pop(&mut self) -> Option<Vec<u8>> {
        self.queue.pop_front().map(|frame| frame.to_bytes())
    }
}

/// The next frame of the session's binary channel. Never completes before the channel
/// is open, or after it has ended.
pub(crate) async fn next_frame(binary: &mut Option<BinaryStream>) -> BinaryFrame {
    if let Some(stream) = binary {
        if let Some(frame) = stream.next().await {
            return frame;
        }
        // Ended (a session that carries no binary frames, or the engine went away,
        // which the event stream reports): stop polling it.
        *binary = None;
    }
    std::future::pending().await
}

#[cfg(test)]
mod tests {
    use super::{FRAME_BUFFER, Frames};
    use jstream_protocol::{
        BINARY_HEADER_LEN, BinaryFrame, BinaryHeader, PreviewCodec, Rev, StreamKind,
    };
    use std::sync::Arc;

    fn frame(channel: u32, keyframe: bool, n: u8) -> BinaryFrame {
        BinaryFrame {
            header: BinaryHeader {
                kind: StreamKind::Preview,
                channel,
                codec: Some(PreviewCodec::H264),
                keyframe,
                end: false,
                width: 2,
                height: 2,
                pts_us: i64::from(n),
                rev: Rev(0),
            },
            payload: Arc::from(&[n][..]),
        }
    }

    /// The payload bytes of every queued frame, in order.
    fn drain(frames: &mut Frames) -> Vec<u8> {
        std::iter::from_fn(|| frames.pop())
            .map(|bytes| bytes[BINARY_HEADER_LEN])
            .collect()
    }

    #[test]
    fn a_full_queue_drops_its_stream_up_to_a_keyframe() {
        let mut frames = Frames::default();
        let n = u8::try_from(FRAME_BUFFER).expect("small");
        for i in 0..n {
            frames.push(frame(1, i == 0, i));
        }
        // Full: this one is dropped, and its stream waits for a keyframe.
        frames.push(frame(1, false, 100));
        assert_eq!(drain(&mut frames), (0..n).collect::<Vec<_>>());
        frames.push(frame(1, false, 101));
        frames.push(frame(1, true, 102));
        frames.push(frame(1, false, 103));
        assert_eq!(drain(&mut frames), [102, 103]);
    }
}
