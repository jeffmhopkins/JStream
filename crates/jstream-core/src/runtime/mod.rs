//! The media pipeline behind `output.start`: capture, one-scene composite, encode,
//! mix, and publish or record. Each stage runs on the thread
//! [ARCH › Threads](../../../docs/ARCHITECTURE.md#threading) assigns.
//!
//! The control thread only starts and stops the pipeline. It does not hold the
//! collection lock across a GPU wait or a thread join (R-RT-002). A publish
//! failure is a channel message; the watcher that receives it settles `failed`. A
//! lost and regained publish link is one too, settled `reconnecting` and `active`.

mod pipeline;
mod publish;
mod render;
mod sink;

pub use pipeline::OutputStats;
#[cfg(test)]
pub(crate) use pipeline::{DrawFn, STOP_DRAIN};
pub(crate) use pipeline::{
    EncodeKey, Encoders, Gate, MediaHost, OpenDraw, Pending, PipelineEvent, Running, cpu_draw,
    launch, launch_program, parts_from,
};
#[cfg(test)]
pub(crate) use publish::tests::serve as serve_rtmp;
pub use publish::{
    LinkState, LivePublish, OpenPublish, PublishError, RecordingPublish, RtmpPublish, connect_rtmp,
};
pub(crate) use render::{Collector, RenderHub, RenderReader, RenderSnapshot};
pub(crate) use sink::Destination;
