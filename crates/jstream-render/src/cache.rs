//! GPU resources kept from frame to frame, keyed by the caller's IDs, and freed only after
//! the GPU has finished the last frame that used them
//! ([ARCH › Threads](../../../docs/ARCHITECTURE.md#threading), "State to RT threads").

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use jstream_platform::GpuFrame;

use crate::compositor::Compositor;
use crate::error::RenderError;
use crate::filter::VideoFilter;
use crate::image::{Alpha, Image};

/// Imports of other textures kept under one key: a platform that delivers frames from a
/// ring of textures gets one bind group per texture, not one per frame.
const KEPT_IMPORTS: usize = 8;

/// The images one compositor draws every frame: uploads of source frames, imports of
/// GPU frames, and the layers nested scenes are composited into. Each is keyed by an ID
/// of the caller's (a source, a scene at a depth), so a frame that shows the same thing
/// again reuses the texture instead of allocating one.
///
/// A frame runs [`Self::begin_frame`], then [`Self::upload`], [`Self::import`] or
/// [`Self::layer`] for everything it draws, then submits its passes and calls [`Self::end_frame`]. An entry
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
    /// Video filters the caller let go of ([`Self::retire_filter`]), each with the last
    /// frame that could have drawn it: a filter can hold textures of its own (a video
    /// delay's ring), so it waits for the fence like an image.
    retired_filters: Vec<(VideoFilter, u64)>,
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
    /// For an import: the imports of the key's other textures, each with the last frame
    /// it was the entry's image. None of them holds a [`jstream_platform::FrameSync`].
    kept: Vec<(Image, u64)>,
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
pub struct Reclaimed {
    images: Vec<Image>,
    filters: Vec<VideoFilter>,
}

impl Reclaimed {
    /// How many images and filters there are.
    pub fn len(&self) -> usize {
        self.images.len() + self.filters.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty() && self.filters.is_empty()
    }

    /// How many of them are video filters.
    pub fn filters(&self) -> usize {
        self.filters.len()
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
            retired_filters: Vec::new(),
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
            entry.image.is_upload()
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

    /// The image for `key` showing `frame`, a frame on this compositor's device, and
    /// marks it used this frame. Nothing is copied: the frame's texture is sampled as
    /// [`Compositor::import_frame`] samples it.
    ///
    /// The image carries the frame's [`jstream_platform::FrameSync`] hold until
    /// [`Self::end_frame`], so each render that draws it stages the hold, and the cache
    /// keeps no hold past the frame: the platform can reuse the texture once the GPU is
    /// done with it. The import itself is kept. A frame in a texture this key showed
    /// recently reuses that texture's image (and bind group), so a platform's ring of
    /// textures costs one import per texture. A frame in a texture of another size
    /// retires the kept imports of the old size.
    ///
    /// # Errors
    /// As for [`Compositor::import`]. The entry is left as it was.
    pub fn import(
        &mut self,
        compositor: &Compositor,
        key: &K,
        frame: &GpuFrame,
    ) -> Result<&Image, RenderError> {
        self.import_as(compositor, key, frame, Alpha::Straight)
    }

    /// [`Self::import`] of a frame whose texture holds sRGB code values already
    /// multiplied by alpha, as a browser page paints them, sampled as
    /// [`Compositor::import_premultiplied`] samples it. An entry under `key` imported
    /// the other way is replaced.
    ///
    /// # Errors
    /// As for [`Compositor::import_premultiplied`]. The entry is left as it was.
    pub fn import_premultiplied(
        &mut self,
        compositor: &Compositor,
        key: &K,
        frame: &GpuFrame,
    ) -> Result<&Image, RenderError> {
        self.import_as(compositor, key, frame, Alpha::PremultipliedSrgb)
    }

    /// [`Self::import`] of a captured frame, sampled as [`Compositor::import_opaque`]
    /// samples it: its alpha is read as 1. An entry under `key` imported another way is
    /// replaced.
    ///
    /// # Errors
    /// As for [`Compositor::import_opaque`]. The entry is left as it was.
    pub fn import_opaque(
        &mut self,
        compositor: &Compositor,
        key: &K,
        frame: &GpuFrame,
    ) -> Result<&Image, RenderError> {
        self.import_as(compositor, key, frame, Alpha::Opaque)
    }

    fn import_as(
        &mut self,
        compositor: &Compositor,
        key: &K,
        frame: &GpuFrame,
        alpha: Alpha,
    ) -> Result<&Image, RenderError> {
        let frame_no = self.frame;
        let texture = frame.texture();
        let reuse = self
            .live
            .get(key)
            .is_some_and(|entry| entry.image.is_import_as(alpha));
        if !reuse {
            let mut image = compositor.import_as(texture.clone(), None, alpha)?;
            image.set_frame(frame);
            return Ok(self.replace(key, image, 0));
        }
        let entry = self.live.get_mut(key).ok_or_else(|| missing("an import"))?;
        if entry.image.texture() != texture {
            let kept = entry
                .kept
                .iter()
                .position(|(image, _)| image.texture() == texture);
            let image = match kept {
                Some(index) => entry.kept.swap_remove(index).0,
                None => compositor.import_as(texture.clone(), None, alpha)?,
            };
            let mut old = std::mem::replace(&mut entry.image, image);
            old.clear_sync();
            entry.kept.push((old, entry.used));
            let size = (texture.width(), texture.height());
            let mut index = 0;
            while index < entry.kept.len() {
                let (image, _) = &entry.kept[index];
                if (image.width(), image.height()) != size {
                    let (image, after) = entry.kept.swap_remove(index);
                    self.retired.push(Retired { image, after });
                } else {
                    index += 1;
                }
            }
            if entry.kept.len() > KEPT_IMPORTS
                && let Some(oldest) = (0..entry.kept.len()).min_by_key(|&i| entry.kept[i].1)
            {
                let (image, after) = entry.kept.swap_remove(oldest);
                self.retired.push(Retired { image, after });
            }
        }
        entry.image.set_frame(frame);
        entry.used = frame_no;
        Ok(&entry.image)
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

    /// The frame being built: what the last [`Self::begin_frame`] returned, or 0 before
    /// the first.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// The live image for `key`, if this cache holds one.
    pub fn get(&self, key: &K) -> Option<&Image> {
        self.live.get(key).map(|entry| &entry.image)
    }

    /// How many images are live (referenced by a recent frame).
    pub fn live(&self) -> usize {
        self.live.len()
    }

    /// How many imports are kept for `key` besides its current image.
    pub fn kept_imports(&self, key: &K) -> usize {
        self.live.get(key).map_or(0, |entry| entry.kept.len())
    }

    /// How many retired images and filters are still waiting for their frame's fence.
    pub fn pending(&self) -> usize {
        self.retired.len() + self.retired_filters.len()
    }

    /// Hands `filter`, which no frame will draw again, to the cache: it is kept until
    /// the GPU has finished the current frame, then handed back in [`Reclaimed`] like a
    /// retired image, so it is dropped where the caller drops those (R-RT-003).
    pub fn retire_filter(&mut self, filter: VideoFilter) {
        self.retired_filters.push((filter, self.frame));
    }

    /// Ends the frame after its passes were submitted. Drops the frame holds of this
    /// frame's imports, retires every entry this frame did not use, arms the frame fence, polls the device without blocking, and hands
    /// back the retired images whose last frame the GPU has finished. `None` when there
    /// are none, so a steady frame allocates nothing here beyond the fence callback.
    pub fn end_frame(&mut self, compositor: &Compositor) -> Option<Reclaimed> {
        let frame = self.frame;
        for entry in self.live.values_mut() {
            entry.image.clear_sync();
        }
        let stale = self.live.iter().any(|(_, entry)| entry.used < frame);
        if stale {
            let live = std::mem::take(&mut self.live);
            for (key, entry) in live {
                if entry.used < frame {
                    self.retire(entry);
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
        if !self.retired.iter().any(|retired| retired.after <= done)
            && !self.retired_filters.iter().any(|(_, after)| *after <= done)
        {
            return None;
        }
        let mut images = Vec::new();
        let mut index = 0;
        while index < self.retired.len() {
            if self.retired[index].after <= done {
                images.push(self.retired.swap_remove(index).image);
            } else {
                index += 1;
            }
        }
        let mut filters = Vec::new();
        let mut index = 0;
        while index < self.retired_filters.len() {
            if self.retired_filters[index].1 <= done {
                filters.push(self.retired_filters.swap_remove(index).0);
            } else {
                index += 1;
            }
        }
        Some(Reclaimed { images, filters })
    }

    /// Everything the cache holds, live or retired, for the caller to drop when the
    /// compositor itself is going away.
    pub fn drain(&mut self) -> Reclaimed {
        let mut all: Vec<Image> = Vec::new();
        for entry in std::mem::take(&mut self.live).into_values() {
            all.push(entry.image);
            all.extend(entry.kept.into_iter().map(|(image, _)| image));
        }
        all.extend(self.retired.drain(..).map(|retired| retired.image));
        Reclaimed {
            images: all,
            filters: self
                .retired_filters
                .drain(..)
                .map(|(filter, _)| filter)
                .collect(),
        }
    }

    fn replace(&mut self, key: &K, image: Image, version: u64) -> &Image {
        let entry = Entry {
            image,
            version,
            used: self.frame,
            kept: Vec::new(),
        };
        let slot = match self.live.entry(key.clone()) {
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                let old = std::mem::replace(slot.get_mut(), entry);
                retire(&mut self.retired, old);
                slot.into_mut()
            }
            std::collections::btree_map::Entry::Vacant(slot) => slot.insert(entry),
        };
        &slot.image
    }
}

impl<K> ResourceCache<K> {
    fn retire(&mut self, entry: Entry) {
        retire(&mut self.retired, entry);
    }
}

/// Retires `entry` and the imports it kept. The holds go now: a retired image is only
/// waiting for the frame fence, which no frame hold is needed for.
fn retire(retired: &mut Vec<Retired>, entry: Entry) {
    let Entry {
        mut image,
        used,
        kept,
        ..
    } = entry;
    image.clear_sync();
    retired.push(Retired { image, after: used });
    retired.extend(
        kept.into_iter()
            .map(|(image, after)| Retired { image, after }),
    );
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
