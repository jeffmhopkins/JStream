//! The media pipeline behind `output.start`: capture, one-scene composite, encode,
//! mix and publish. Each stage runs on the thread
//! [ARCH › Threads](../../../docs/ARCHITECTURE.md#threading) assigns.
//!
//! The control thread only starts and stops the pipeline. It does not hold the
//! collection lock across a GPU wait or a thread join (R-RT-002).

mod pipeline;
mod publish;

pub use pipeline::OutputStats;
pub(crate) use pipeline::{MediaHost, Running, launch, parts_from};
pub use publish::{LivePublish, PublishError, RecordingPublish, RtmpPublish, connect_rtmp};
