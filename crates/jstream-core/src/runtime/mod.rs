//! The media pipeline behind `output.start`: capture, one-scene composite, encode,
//! mix and publish. Each stage runs on the thread
//! [ARCH › Threads](../../../docs/ARCHITECTURE.md#threading) assigns.
//!
//! The control thread only starts and stops the pipeline. It does not hold the
//! collection lock across a GPU wait or a thread join (R-RT-002). A publish
//! failure is a channel message; the watcher that receives it settles `failed`. A
//! lost and regained publish link is one too, settled `reconnecting` and `active`.

mod pipeline;
mod publish;

pub use pipeline::OutputStats;
#[cfg(test)]
pub(crate) use pipeline::{DrawFn, OpenDraw};
pub(crate) use pipeline::{Gate, MediaHost, Pending, PipelineEvent, Running, launch, parts_from};
#[cfg(test)]
pub(crate) use publish::tests::serve as serve_rtmp;
pub use publish::{
    LinkState, LivePublish, OpenPublish, PublishError, RecordingPublish, RtmpPublish, connect_rtmp,
};
