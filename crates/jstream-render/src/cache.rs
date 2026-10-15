//! GPU resources kept from frame to frame, keyed by the caller's IDs, and freed only after
//! the GPU has finished the last frame that used them
//! ([ARCH › Threads](../../../docs/ARCHITECTURE.md#threading), "State to RT threads").

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::compositor::Compositor;
use crate::error::RenderError;
use crate::image::Image;

/// The images one compositor draws every frame: uploads of source frames, and the
/// layers nested scenes are composited into. Each is keyed by an ID of the caller's
/// (a source, a scene at a depth), so a frame that shows the same thing again reuses
/// the texture instead of allocating one.
///
/// A frame runs [`Self::begin_frame`], then [`Self::upload`] or [`Self::layer`] for
/// everything it draws, then submits its passes and calls [`Self::end_frame`]. An entry
/// the frame did not touch is no longer referenced, so it is retired: kept until the
/// GPU has finished every frame that used it (the frame fence, a
/// `Queue::on_submitted_work_done` callback), then handed back by `end_frame` as
/// [`Reclaimed`], so the caller decides which thread drops it (R-RT-003).
///
/// Nothing here waits on the GPU. [`Self::end_frame`] polls the device once without
/// blocking, so a fence that has passed is noticed even on a tick that reads nothing
/// back.
pub struct ResourceCache<K> {
    live: BTreeMap<K, Entry>,
    retired: Vec<Retired>,
    /// The frame being built. 0 before the first [`Self::begin_frame`].
    frame: u64,
    /// The newest frame whose submitted work the GPU has finished.
    done: Arc<AtomicU64>,
}

struct Entry {
    image: Image,
    /// What the caller said the contents are: an upload is rewritten when it changes.
    version: u64,
    /// The last frame that referenced this entry.
    used: u64,
}

/// An image no frame references any more, waiting for the GPU to finish `after`.
struct Retired {
    image: Image,
    after: u64,
}

/// Images whose last frame the GPU has finished, for the caller to drop where dropping
/// is allowed (on a collector thread, when the cache lives on a real-time thread).
#[derive(Debug)]
#[must_use = "dropping it here frees the textures on this thread"]
pub struct Reclaimed(Vec<Image>);

impl Reclaimed {
    /// How many images there are.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<K: Ord + Clone> Default for ResourceCache<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Ord + Clone> ResourceCache<K> {
    /// An empty cache.
    pub fn new() -> Self {
        Self {
            live: BTreeMap::new(),
            retired: Vec::new(),
            frame: 0,
            done: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Starts the next frame. Returns its number, counting from 1.
    pub fn begin_frame(&mut self) -> u64 {
        self.frame += 1;
        self.frame
    }

    /// The image for `key`, holding `rgba` (tightly packed straight sRGB RGBA8) at
    /// `width` by `height`, and marks it used this frame.
    ///
    /// An entry with the same `version` and size is reused as it is, so a frame that
    /// shows an unchanged source uploads nothing. A new version at the same size is
    /// written into the same texture, after every submit that sampled the old one. A new
    /// size makes a new texture, and the old one is retired.
    ///
    /// # Errors
    /// As for [`Compositor::upload_rgba8`]. The entry is left as it was.
    pub fn upload(
        &mut self,
        compositor: &Compositor,
        key: &K,
        version: u64,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<&Image, RenderError> {
        let frame = self.frame;
        let reuse = self.live.get(key).is_some_and(|entry| {
            entry.image.target().is_none()
                && entry.image.width() == width
                && entry.image.height() == height
        });
        if reuse {
            let entry = self.live.get_mut(key).ok_or_else(|| missing("an upload"))?;
            if entry.version != version {
                compositor.rewrite_rgba8(&entry.image, rgba)?;
                entry.version = version;
            }
            entry.used = frame;
            return Ok(&entry.image);
        }
        let image = compositor.upload_rgba8(width, height, rgba)?;
        Ok(self.replace(key, image, version))
    }

    /// The layer for `key` at `width` by `height`, for [`Compositor::render_into`], and
    /// marks it used this frame. A layer of the same size is reused; its contents are
    /// whatever was last drawn into it.
    ///
    /// # Errors
    /// As for [`Compositor::layer`].
    pub fn layer(
        &mut self,
        compositor: &Compositor,
        key: &K,
        width: u32,
        height: u32,
    ) -> Result<&Image, RenderError> {
        let frame = self.frame;
        let reuse = self.live.get(key).is_some_and(|entry| {
            entry.image.target().is_some()
                && entry.image.width() == width
                && entry.image.height() == height
        });
        if reuse {
            let entry = self.live.get_mut(key).ok_or_else(|| missing("a layer"))?;
            entry.used = frame;
            return Ok(&entry.image);
        }
        let image = compositor.layer(width, height)?;
        Ok(self.replace(key, image, 0))
    }

    /// The live image for `key`, if this cache holds one.
    pub fn get(&self, key: &K) -> Option<&Image> {
        self.live.get(key).map(|entry| &entry.image)
    }

    /// How many images are live (referenced by a recent frame).
    pub fn live(&self) -> usize {
        self.live.len()
    }

    /// How many retired images are still waiting for their frame's fence.
    pub fn pending(&self) -> usize {
        self.retired.len()
    }

    /// Ends the frame after its passes were submitted. Retires every entry this frame
    /// did not use, arms the frame fence, polls the device without blocking, and hands
    /// back the retired images whose last frame the GPU has finished. `None` when there
    /// are none, so a steady frame allocates nothing here beyond the fence callback.
    pub fn end_frame(&mut self, compositor: &Compositor) -> Option<Reclaimed> {
        let frame = self.frame;
        let stale = self.live.iter().any(|(_, entry)| entry.used < frame);
        if stale {
            let live = std::mem::take(&mut self.live);
            for (key, entry) in live {
                if entry.used < frame {
                    self.retired.push(Retired {
                        image: entry.image,
                        after: entry.used,
                    });
                } else {
                    self.live.insert(key, entry);
                }
            }
        }
        let done = Arc::clone(&self.done);
        compositor.queue().on_submitted_work_done(move || {
            done.fetch_max(frame, Ordering::AcqRel);
        });
        // A failed poll means a lost device; the next submit reports that.
        let _ = compositor.device().poll(wgpu::PollType::Poll);
        self.reclaim()
    }

    /// The retired images whose fence has passed, without ending a frame.
    pub fn reclaim(&mut self) -> Option<Reclaimed> {
        let done = self.done.load(Ordering::Acquire);
        if !self.retired.iter().any(|retired| retired.after <= done) {
            return None;
        }
        let mut freed = Vec::new();
        let mut index = 0;
        while index < self.retired.len() {
            if self.retired[index].after <= done {
                freed.push(self.retired.swap_remove(index).image);
            } else {
                index += 1;
            }
        }
        Some(Reclaimed(freed))
    }

    /// Everything the cache holds, live or retired, for the caller to drop when the
    /// compositor itself is going away.
    pub fn drain(&mut self) -> Reclaimed {
        let mut all: Vec<Image> = std::mem::take(&mut self.live)
            .into_values()
            .map(|entry| entry.image)
            .collect();
        all.extend(self.retired.drain(..).map(|retired| retired.image));
        Reclaimed(all)
    }

    fn replace(&mut self, key: &K, image: Image, version: u64) -> &Image {
        let entry = Entry {
            image,
            version,
            used: self.frame,
        };
        let slot = match self.live.entry(key.clone()) {
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                let old = std::mem::replace(slot.get_mut(), entry);
                self.retired.push(Retired {
                    image: old.image,
                    after: old.used,
                });
                slot.into_mut()
            }
            std::collections::btree_map::Entry::Vacant(slot) => slot.insert(entry),
        };
        &slot.image
    }
}

fn missing(what: &str) -> RenderError {
    RenderError::Invalid {
        what: "cache",
        detail: format!("{what} vanished between lookup and use"),
    }
}

impl<K> std::fmt::Debug for ResourceCache<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceCache")
            .field("live", &self.live.len())
            .field("retired", &self.retired.len())
            .field("frame", &self.frame)
            .finish_non_exhaustive()
    }
}
