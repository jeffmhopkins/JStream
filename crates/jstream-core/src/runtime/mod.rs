//! The media pipeline behind `output.start`: capture, a composite of each video mix
//! the outputs show, encode, the audio mix, and publish or record. Each stage runs on the thread
//! [ARCH › Threads](../../../docs/ARCHITECTURE.md#threading) assigns.
//!
//! The control thread only starts and stops the pipeline. It does not hold the
//! collection lock across a GPU wait or a thread join (R-RT-002). A publish
//! failure is a channel message; the watcher that receives it settles `failed`. A
//! lost and regained publish link is one too, settled `reconnecting` and `active`.

mod captures;
mod pipeline;
mod publish;
mod render;
mod sink;
mod vcam;

#[cfg(all(test, windows, feature = "hw-tests"))]
pub(crate) use captures::Backends as CaptureBackends;
#[cfg(test)]
pub(crate) use captures::{RETRY_MS, start as start_capture};
pub use pipeline::OutputStats;
#[cfg(test)]
pub(crate) use pipeline::{DrawFn, STOP_DRAIN, gpu_draw, open_capture};
pub(crate) use pipeline::{
    EncodeKey, Encoders, FRAME_DEN, FRAME_NUM, Gate, MediaHost, MixKey, OpenDraw, Pending,
    PipelineEvent, Running, cpu_draw, launch, launch_program, launch_vcam, parts_from,
};
#[cfg(test)]
pub(crate) use publish::tests::serve as serve_rtmp;
pub use publish::{
    LinkState, LivePublish, OpenPublish, PublishError, RecordingPublish, RtmpPublish, connect_rtmp,
};
pub(crate) use render::{Collector, Garbage, RenderHub, RenderReader, RenderSnapshot};
pub(crate) use sink::Destination;
pub(crate) use vcam::VcamOut;
