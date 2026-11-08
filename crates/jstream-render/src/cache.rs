//! GPU resources kept from frame to frame, keyed by the caller's IDs, and freed only after
//! the GPU has finished the last frame that used them
//! ([ARCH › Threads](../../../docs/ARCHITECTURE.md#threading), "State to RT threads").

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use jstream_media::FrameSize;
use jstream_platform::GpuFrame;

use crate::compositor::Compositor;
use crate::error::RenderError;
use crate::filter::VideoFilter;
use crate::image::{Alpha, Image};

/// Imports of other textures kept under one key: a platform that delivers frames from a
/// ring of textures gets one bind group per texture, not one per frame.
const KEPT_IMPORTS: usize = 8;

/// How much more resolution than its draws needed an upload drawn small is held at
/// ([`ResourceCache::upload_drawn`]), before rounding up to a power of two: an item
/// enlarged a little since needs no new upload.
const DRAWN_HEADROOM: f32 = 1.25;

/// The smallest fraction of its frame's size, each way, an upload drawn small is held
/// at. A texel of the upload averages the frame's texels under it, so this bounds that
/// work to 16 × 16 of them; an item drawn smaller still samples the upload's mip chain.
const DRAWN_MIN_SCALE: f32 = 1.0 / 16.0;

/// A frame with fewer texels than this is always uploaded whole: holding it smaller
/// saves less than half a megabyte.
const DRAWN_MIN_TEXELS: u64 = 256 * 256;

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
    /// Whether [`Self::upload_drawn`] holds an upload at the size it is drawn
    /// ([`Self::draw_uploads_small`]).
    drawn_small: bool,
    retired: Vec<Retired>,
    /// Video filters the caller let go of ([`Self::retire_filter`]), each with the last
    /// frame that could have drawn it: a filter can hold textures of its own (a video
    /// delay's ring), so it waits for the fence like an image.
    retired_filters: Vec<(VideoFilter, u64)>,
    /// What [`Self::keep_while`] replaced, handed back with the next reclaim.
    released: Vec<Arc<AtomicBool>>,
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
    /// For an upload, the fraction of its frame's size it is held at each way, 1 for
    /// the whole frame ([`ResourceCache::upload_drawn`]).
    scale: f32,
    /// Kept while no frame uses it, for as long as this is true
    /// ([`ResourceCache::keep_while`]).
    alive: Option<Arc<AtomicBool>>,
}

/// An image no frame references any more, waiting for the GPU to finish `after`.
struct Retired {
    image: Image,
    after: u64,
    /// What kept it ([`Entry::alive`]), freed with it, never on the cache's thread.
    alive: Option<Arc<AtomicBool>>,
}

/// Images whose last frame the GPU has finished, for the caller to drop where dropping
/// is allowed (on a collector thread, when the cache lives on a real-time thread).
#[derive(Debug)]
#[must_use = "dropping it here frees the textures on this thread"]
pub struct Reclaimed {
    images: Vec<Image>,
    filters: Vec<VideoFilter>,
    /// What kept them ([`ResourceCache::keep_while`]): the cache may hold the last
    /// reference.
    alive: Vec<Arc<AtomicBool>>,
}

impl Reclaimed {
    /// How many images and filters there are, and keep-alive flags
    /// ([`ResourceCache::keep_while`]).
    pub fn len(&self) -> usize {
        self.images.len() + self.filters.len() + self.alive.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty() && self.filters.is_empty() && self.alive.is_empty()
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
            drawn_small: false,
            retired: Vec::new(),
            retired_filters: Vec::new(),
            released: Vec::new(),
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
        self.upload_if_needed(compositor, key, version, (width, height), || Some(rgba))?
            .ok_or_else(|| missing("an upload"))
    }

    /// [`Self::upload`] of pixels read only if they are needed: `rgba` is called only
    /// when the entry doesn't hold `version` at this size already, so a caller that
    /// keeps its pixels elsewhere (a still whose copy was let go of, #1139) need not
    /// have them at hand. `Ok(None)` when it was called and gave none: the entry is
    /// left as it was, and marked used if there is one, so the image it held (if any)
    /// is still drawn until the pixels are given.
    ///
    /// # Errors
    /// As for [`Self::upload`].
    pub fn upload_if_needed<B: AsRef<[u8]>>(
        &mut self,
        compositor: &Compositor,
        key: &K,
        version: u64,
        (width, height): (u32, u32),
        rgba: impl FnOnce() -> Option<B>,
    ) -> Result<Option<&Image>, RenderError> {
        let frame = self.frame;
        // By the frame it holds and the texture's own size: one held small by
        // `upload_drawn` is made again whole.
        let reuse = self.live.get(key).is_some_and(|entry| {
            entry.image.is_upload()
                && entry.image.full() == (width, height)
                && (entry.image.width(), entry.image.height()) == (width, height)
        });
        if reuse {
            let entry = self.live.get_mut(key).ok_or_else(|| missing("an upload"))?;
            entry.used = frame;
            if entry.version != version {
                let Some(rgba) = rgba() else {
                    return Ok(None);
                };
                compositor.rewrite_rgba8(&entry.image, rgba.as_ref())?;
                entry.version = version;
            }
            return Ok(Some(&entry.image));
        }
        let Some(rgba) = rgba() else {
            self.keep(key);
            return Ok(None);
        };
        let image = compositor.upload_rgba8(width, height, rgba.as_ref())?;
        Ok(Some(self.replace(key, image, version)))
    }

    /// Keeps the entry under `key`, if there is one, while no frame uses it, for as long
    /// as `alive` is true, so a still whose pixels its caller let go of is drawn at once
    /// when it is shown again (#1139). The entry keeps `alive` until it is replaced or
    /// retired, and hands it back in [`Reclaimed`] then, so the cache never drops the
    /// last reference itself.
    pub fn keep_while(&mut self, key: &K, alive: &Arc<AtomicBool>) {
        if let Some(entry) = self.live.get_mut(key)
            && !entry
                .alive
                .as_ref()
                .is_some_and(|held| Arc::ptr_eq(held, alive))
        {
            // The one it replaces goes out with the next reclaim, as a retired one does.
            self.released.extend(entry.alive.replace(Arc::clone(alive)));
        }
    }

    /// Marks the entry under `key`, if there is one, used this frame.
    fn keep(&mut self, key: &K) {
        let frame = self.frame;
        if let Some(entry) = self.live.get_mut(key) {
            entry.used = frame;
        }
    }

    /// Lets [`Self::upload_drawn`] hold an upload at the size it is drawn (#1139). Off
    /// by default, so a cache that renders a deterministic frame (a snapshot, a frame
    /// sheet) draws every upload from its whole frame.
    pub fn draw_uploads_small(&mut self, on: bool) {
        self.drawn_small = on;
    }

    /// [`Self::upload`] of a `width` × `height` frame that only items draw (not a video chain, which reads
    /// its texels as they are), held at the size the compositor drew it at over the
    /// frames before, once [`Self::draw_uploads_small`] allows it (#1139, ADR-0117 §2).
    ///
    /// `planned` is what the caller's placements say its items need: the fraction of the
    /// frame's resolution they draw, over every mix whether or not it draws this frame
    /// (0 or less when unknown). A new entry is held at it (whole when unknown). After
    /// that, each frame reads how much of
    /// the frame's resolution its draws needed last time (the most any item sampled; a
    /// 3D placement needs it all), takes the larger of that and `planned` (so a mix
    /// drawn only every other tick, or a cut to a larger placement, is never drawn from
    /// too few texels), and holds the upload at a power-of-two fraction of
    /// the frame's size with a quarter of headroom over that, no smaller than a
    /// sixteenth: each texel is the mean of the frame's texels under it. An item drawn
    /// larger than that live (a gesture, a camera) gets a larger upload the next frame,
    /// and one drawn at less than
    /// half of it a smaller one, so a small change in size uploads nothing. The image is
    /// placed at the frame's size whatever its texels, as [`Image::set_extent`] places an
    /// image drawn finer.
    ///
    /// # Errors
    /// As for [`Self::upload`].
    pub fn upload_drawn(
        &mut self,
        compositor: &Compositor,
        key: &K,
        version: u64,
        size: (u32, u32),
        rgba: &[u8],
        planned: f32,
    ) -> Result<&Image, RenderError> {
        self.upload_drawn_if_needed(compositor, key, version, size, planned, || Some(rgba))?
            .ok_or_else(|| missing("an upload"))
    }

    /// [`Self::upload_drawn`] of pixels read only if they are needed, as
    /// [`Self::upload_if_needed`] reads them. When a larger upload was due and `rgba`
    /// gave none, the entry keeps the texels it holds and is drawn from them.
    ///
    /// # Errors
    /// As for [`Self::upload`].
    pub fn upload_drawn_if_needed<B: AsRef<[u8]>>(
        &mut self,
        compositor: &Compositor,
        key: &K,
        version: u64,
        (width, height): (u32, u32),
        planned: f32,
        rgba: impl FnOnce() -> Option<B>,
    ) -> Result<Option<&Image>, RenderError> {
        let small = u64::from(width) * u64::from(height) < DRAWN_MIN_TEXELS;
        if !self.drawn_small || small {
            return self.upload_if_needed(compositor, key, version, (width, height), rgba);
        }
        let frame = self.frame;
        let held = self.live.get(key).and_then(|entry| {
            (entry.image.is_upload() && entry.image.full() == (width, height))
                .then_some(entry.scale)
        });
        let scale = match held {
            None if planned > 0.0 => drawn_scale(planned),
            None => 1.0,
            Some(held) => {
                let entry = self.live.get_mut(key).ok_or_else(|| missing("an upload"))?;
                // Read once a frame, so every mix of the frame draws the same texels,
                // and never under what the placements plan: a mix that isn't due on
                // a tick draws nothing then, and must not shrink what it shows.
                let need = if entry.used == frame {
                    0.0
                } else {
                    entry.image.take_need().max(planned.max(0.0))
                };
                let wanted = drawn_scale(need);
                if need == 0.0 || (need <= held && wanted > held / 2.0) {
                    held
                } else {
                    wanted
                }
            }
        };
        if held == Some(scale) {
            let entry = self.live.get_mut(key).ok_or_else(|| missing("an upload"))?;
            entry.used = frame;
            if entry.version != version {
                let Some(rgba) = rgba() else {
                    return Ok(None);
                };
                compositor.rewrite_rgba8(&entry.image, rgba.as_ref())?;
                entry.version = version;
            }
            return Ok(Some(&entry.image));
        }
        let Some(rgba) = rgba() else {
            self.keep(key);
            return Ok(None);
        };
        let to = (scaled(width, scale), scaled(height, scale));
        let image = compositor.upload_rgba8_into(width, height, rgba.as_ref(), to)?;
        self.replace(key, image, version);
        let entry = self.live.get_mut(key).ok_or_else(|| missing("an upload"))?;
        entry.scale = scale;
        Ok(Some(&entry.image))
    }

    /// [`Self::upload`] of a CPU video frame read as opaque, converted on the GPU as
    /// [`Compositor::upload_frame`] converts it: packed RGBA8 or BGRA8, or NV12.
    ///
    /// # Errors
    /// As for [`Compositor::upload_frame`]. The entry is left as it was.
    pub fn upload_frame(
        &mut self,
        compositor: &Compositor,
        key: &K,
        version: u64,
        frame: &jstream_media::VideoFrame,
    ) -> Result<&Image, RenderError> {
        let size = frame.info().size;
        let at = self.frame;
        let reuse = self.live.get(key).is_some_and(|entry| {
            entry.image.is_upload()
                && entry.image.full() == (size.width(), size.height())
                && entry.image.width() == size.width()
                && entry.image.height() == size.height()
        });
        if reuse {
            let entry = self.live.get_mut(key).ok_or_else(|| missing("an upload"))?;
            if entry.version != version {
                compositor.rewrite_frame(&entry.image, frame)?;
                entry.version = version;
            }
            entry.used = at;
            return Ok(&entry.image);
        }
        let image = compositor.upload_frame(frame)?;
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
                    self.retired.push(Retired {
                        image,
                        after,
                        alive: None,
                    });
                } else {
                    index += 1;
                }
            }
            if entry.kept.len() > KEPT_IMPORTS
                && let Some(oldest) = (0..entry.kept.len()).min_by_key(|&i| entry.kept[i].1)
            {
                let (image, after) = entry.kept.swap_remove(oldest);
                self.retired.push(Retired {
                    image,
                    after,
                    alive: None,
                });
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

    /// Places `key`'s image as if it were `extent` ([`Image::set_extent`]), if there is
    /// one. An upload or a layer made again, at a new size, is placed at its own size
    /// until this is called again.
    pub fn set_extent(&mut self, key: &K, extent: FrameSize) {
        if let Some(entry) = self.live.get_mut(key) {
            entry.image.set_extent(extent);
        }
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

    /// Hands `image`, a handle to a texture something stops holding, to the cache: it is
    /// kept until the GPU has finished the current frame, then handed back in
    /// [`Reclaimed`], so a texture that was its last handle is freed where the caller
    /// drops those (R-RT-003).
    pub fn retire_image(&mut self, image: Image) {
        self.retired.push(Retired {
            image,
            after: self.frame,
            alive: None,
        });
    }

    /// Ends the frame after its passes were submitted. Drops the frame holds of this
    /// frame's imports, retires every entry this frame did not use (but those
    /// [`Self::keep_while`] keeps), arms the frame fence, polls the device without blocking, and hands
    /// back the retired images whose last frame the GPU has finished. `None` when there
    /// are none, so a steady frame allocates nothing here beyond the fence callback.
    pub fn end_frame(&mut self, compositor: &Compositor) -> Option<Reclaimed> {
        let frame = self.frame;
        for entry in self.live.values_mut() {
            entry.image.clear_sync();
        }
        let stale = |entry: &Entry| {
            entry.used < frame
                && !entry
                    .alive
                    .as_ref()
                    .is_some_and(|alive| alive.load(Ordering::Acquire))
        };
        if self.live.values().any(stale) {
            let live = std::mem::take(&mut self.live);
            for (key, entry) in live {
                if stale(&entry) {
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
            && self.released.is_empty()
        {
            return None;
        }
        let mut images = Vec::new();
        let mut alive = std::mem::take(&mut self.released);
        let mut index = 0;
        while index < self.retired.len() {
            if self.retired[index].after <= done {
                let retired = self.retired.swap_remove(index);
                images.push(retired.image);
                alive.extend(retired.alive);
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
        Some(Reclaimed {
            images,
            filters,
            alive,
        })
    }

    /// Everything the cache holds, live or retired, for the caller to drop when the
    /// compositor itself is going away.
    pub fn drain(&mut self) -> Reclaimed {
        let mut all: Vec<Image> = Vec::new();
        let mut alive = std::mem::take(&mut self.released);
        for entry in std::mem::take(&mut self.live).into_values() {
            all.push(entry.image);
            all.extend(entry.kept.into_iter().map(|(image, _)| image));
            alive.extend(entry.alive);
        }
        for retired in self.retired.drain(..) {
            all.push(retired.image);
            alive.extend(retired.alive);
        }
        Reclaimed {
            alive,
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
            scale: 1.0,
            alive: None,
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

/// The fraction of its frame's size, each way, an upload is held at when its draws
/// needed `need` of it ([`ResourceCache::upload_drawn`]).
fn drawn_scale(need: f32) -> f32 {
    if need.is_nan() || need * DRAWN_HEADROOM >= 1.0 {
        return 1.0;
    }
    let steps = (need * DRAWN_HEADROOM).log2().ceil();
    2f32.powf(steps).clamp(DRAWN_MIN_SCALE, 1.0)
}

/// `side` texels at `scale` of them, at least one.
fn scaled(side: u32, scale: f32) -> u32 {
    if scale >= 1.0 {
        return side;
    }
    ((side as f32 * scale).ceil() as u32).clamp(1, side)
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
        alive,
        ..
    } = entry;
    image.clear_sync();
    retired.push(Retired {
        image,
        after: used,
        alive,
    });
    retired.extend(kept.into_iter().map(|(image, after)| Retired {
        image,
        after,
        alive: None,
    }));
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
