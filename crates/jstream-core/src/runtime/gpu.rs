//! One engine GPU context, lent to every compositor and to the browser engine
//! ([`SharedGpu`], ADR-0117 §5).
//!
//! The platform's GPU interop opens a device on the media adapter. Before this, each
//! holder opened its own: the host's browser device, every program stage, the preview
//! hub and the snapshot thread, each on a wgpu instance of its own. A browser page's
//! frames are textures on whichever device the browser engine was last given, and a
//! compositor draws a texture only on its own device, so the preview and snapshots drew
//! pages as nothing, and each stage start moved the pages to a new device (#1194).
//!
//! The engine wraps its interop in [`SharedGpu`]: the first open opens the device and
//! keeps it, and every later open lends a clone of the same context (same
//! [`ContextKey`](jstream_platform::ContextKey)). The context is opened again only once
//! it is lost: a lent context whose device the compositors' shared watch reports lost
//! ([`jstream_render::context_lost`]) is forgotten, and the next open gets a new one.
//! Holders that drew on the lost one follow on their own: the program stage falls back
//! as before and the next one opens on the new context, and the preview and snapshot
//! compositors reopen. The engine is told of each new context that replaces a lost one
//! ([`SharedGpu::on_replaced`]) and hands it to the browser engine, so pages move to it
//! whichever holder saw the loss first. A failed open is not kept: the next one tries
//! again.

use std::sync::{Arc, Mutex};

use jstream_media::FrameSize;
use jstream_platform::{GpuContext, GpuFrame, GpuInterop, PlatformError};

use crate::lock;

/// Told of each context that replaces a lost one.
pub(crate) type Replaced = Arc<dyn Fn(&GpuContext) + Send + Sync>;

/// The engine's GPU interop, opening the platform's context once and lending it
/// ([`GpuInterop::open_device`] returns a clone of the context it holds).
#[derive(Default)]
pub(crate) struct SharedGpu {
    /// The platform's interop, once the host gave the engine its backends.
    inner: Mutex<Option<Arc<dyn GpuInterop>>>,
    /// The context lent now. Held across an open, so two holders opening at once get
    /// the same one.
    current: Mutex<Option<GpuContext>>,
    /// Told of a new context that replaces a lost one, with no lock of this held.
    replaced: Mutex<Option<Replaced>>,
}

impl SharedGpu {
    /// Calls `replaced` with each new context opened in place of a lost one, on the
    /// thread that opened it, with none of this type's locks held: the engine hands it
    /// to the browser engine.
    pub(crate) fn on_replaced(&self, replaced: Replaced) {
        *lock(&self.replaced) = Some(replaced);
    }

    /// Opens contexts through `inner` from now on. A context lent from an earlier one is
    /// forgotten.
    pub(crate) fn set_interop(&self, inner: Arc<dyn GpuInterop>) {
        *lock(&self.inner) = Some(inner);
        *lock(&self.current) = None;
    }

    /// The context, opened now if none is held or the one held was lost. `None`, logged
    /// at `debug`, when the engine has no interop or it opens no device: the holder then
    /// opens a device of its own, as without interop.
    pub(crate) fn context(&self) -> Option<GpuContext> {
        self.open()
            .inspect_err(|err| tracing::debug!(error = %err, "no shared GPU context"))
            .ok()
    }

    /// [`Self::context`], with why there is none.
    fn open(&self) -> Result<GpuContext, PlatformError> {
        let inner = lock(&self.inner)
            .clone()
            .ok_or_else(|| PlatformError::Unsupported {
                what: "GPU interop (the engine has no backends yet)".into(),
            })?;
        let context = {
            let mut current = lock(&self.current);
            let mut lost = false;
            if let Some(context) = current.as_ref() {
                match jstream_render::context_lost(context) {
                    None => return Ok(context.clone()),
                    Some(why) => {
                        tracing::info!(reason = %why, "the engine's GPU context was lost; opening a new one");
                        *current = None;
                        lost = true;
                    }
                }
            }
            let context = inner.open_device()?;
            tracing::info!(adapter = %context.adapter().get_info().name, "engine GPU context opened");
            *current = Some(context.clone());
            (context, lost)
        };
        let (context, lost) = context;
        if lost {
            let replaced = lock(&self.replaced).clone();
            if let Some(replaced) = replaced {
                replaced(&context);
            }
        }
        Ok(context)
    }

    /// The context held now, without opening one: `None` before the first open and after
    /// a loss was seen.
    #[cfg(test)]
    pub(crate) fn held(&self) -> Option<GpuContext> {
        lock(&self.current).clone()
    }
}

impl GpuInterop for SharedGpu {
    fn open_device(&self) -> Result<GpuContext, PlatformError> {
        self.open()
    }

    fn encode_target(
        &self,
        context: &GpuContext,
        size: FrameSize,
    ) -> Result<GpuFrame, PlatformError> {
        let inner = lock(&self.inner)
            .clone()
            .ok_or(PlatformError::InvalidState {
                what: "encode_target",
                state: "the engine has no GPU interop",
            })?;
        inner.encode_target(context, size)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use jstream_platform::AdapterId;
    use jstream_render::{Compositor, OpenOptions};

    use super::*;

    /// Interop on a software device that counts its opens.
    #[derive(Default)]
    struct Counting {
        opens: AtomicUsize,
    }

    impl GpuInterop for Counting {
        fn open_device(&self) -> Result<GpuContext, PlatformError> {
            let compositor = Compositor::open(OpenOptions::headless()).map_err(|err| {
                PlatformError::Unsupported {
                    what: format!("a software adapter: {err}"),
                }
            })?;
            self.opens.fetch_add(1, Ordering::Relaxed);
            Ok(GpuContext::new(
                compositor.adapter().clone(),
                compositor.device().clone(),
                compositor.queue().clone(),
                AdapterId {
                    luid: None,
                    vendor: 0,
                },
                None,
            ))
        }

        fn encode_target(
            &self,
            _context: &GpuContext,
            _size: FrameSize,
        ) -> Result<GpuFrame, PlatformError> {
            Err(PlatformError::Unsupported {
                what: "encode targets".into(),
            })
        }
    }

    /// A shared GPU on [`Counting`], or `None` (logged) on a machine with no adapter.
    fn shared() -> Option<(SharedGpu, Arc<Counting>)> {
        let counting = Arc::new(Counting::default());
        let gpu = SharedGpu::default();
        gpu.set_interop(Arc::clone(&counting) as Arc<dyn GpuInterop>);
        if gpu.context().is_none() {
            assert!(std::env::var_os("CI").is_none(), "CI has no wgpu adapter");
            let _ = writeln!(
                std::io::stderr(),
                "jstream-core: skipping shared GPU test, no adapter"
            );
            return None;
        }
        Some((gpu, counting))
    }

    #[test]
    fn without_interop_there_is_no_context() {
        let gpu = SharedGpu::default();
        assert!(gpu.context().is_none());
        assert!(gpu.open_device().is_err());
    }

    /// Every holder gets the same context, so a texture made for one draws on all of
    /// them (#1194).
    #[test]
    fn every_open_lends_the_one_context() {
        let Some((gpu, counting)) = shared() else {
            return;
        };
        let first = gpu.context().expect("context");
        let second = gpu.open_device().expect("context");
        assert_eq!(first.key(), second.key());
        let stage = Compositor::on_context(&first);
        let preview = Compositor::on_context(&second);
        assert_eq!(stage.context(), preview.context());
        assert_eq!(counting.opens.load(Ordering::Relaxed), 1, "opened once");
    }

    /// A lost context is forgotten: the next open gets a new one, and the engine is told,
    /// once, whichever compositor saw the loss.
    #[test]
    fn a_lost_context_is_replaced_and_reported() {
        let Some((gpu, counting)) = shared() else {
            return;
        };
        let told = Arc::new(Mutex::new(Vec::new()));
        gpu.on_replaced(Arc::new({
            let told = Arc::clone(&told);
            move |context: &GpuContext| lock(&told).push(context.key())
        }));
        let first = gpu.context().expect("context");
        // Two compositors share the context's watch, so the first one's sees the loss
        // even though the second was opened last.
        let stage = Compositor::on_context(&first);
        let snapshots = Compositor::on_context(&first);
        first.device().destroy();
        assert!(stage.lost().is_some(), "the first compositor sees the loss");
        assert!(snapshots.lost().is_some());
        let second = gpu.context().expect("a new context");
        assert_ne!(first.key(), second.key());
        assert_eq!(gpu.held().map(|held| held.key()), Some(second.key()));
        assert_eq!(counting.opens.load(Ordering::Relaxed), 2);
        assert_eq!(*lock(&told), [second.key()]);
        // The new one is lent from then on.
        assert_eq!(gpu.context().expect("context").key(), second.key());
        assert_eq!(lock(&told).len(), 1);
    }
}
