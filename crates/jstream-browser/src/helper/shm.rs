//! A page's slot files (Linux, ADR-0095): CEF's software paints, as straight-alpha
//! RGBA8, written into the next of [`SLOTS`] files the engine reads them from
//! ([`crate::cpu_frames`]), in the private folder it made for this helper.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use crate::cpu_frames::{SLOTS, frame_bytes, handle, slot_file, straight_rgba};

/// A page's slot files.
#[derive(Default)]
pub(crate) struct PageTextures(Mutex<Ring>);

#[derive(Default)]
struct Ring {
    files: [Option<File>; SLOTS],
    /// Slots the engine hasn't released.
    busy: [bool; SLOTS],
    next: usize,
    /// The converted paint, kept for the next.
    rgba: Vec<u8>,
}

/// One paint written into a slot file.
pub(crate) struct Painted {
    pub(crate) slot: u8,
    pub(crate) handle: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl PageTextures {
    /// The engine is done with `slot`.
    pub(crate) fn release(&self, slot: u8) {
        let mut ring = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(busy) = ring.busy.get_mut(usize::from(slot)) {
            *busy = false;
        }
    }

    /// Writes CEF's paint `bgra` (premultiplied BGRA8 of `size`) into page `page`'s next
    /// free slot file in `dir`. `Ok(None)` when the engine still holds every slot: the
    /// paint is skipped.
    pub(crate) fn paint(
        &self,
        dir: &Path,
        page: u32,
        bgra: &[u8],
        size: (u32, u32),
    ) -> Result<Option<Painted>, String> {
        let bytes = frame_bytes(size.0, size.1)
            .filter(|&bytes| bytes <= bgra.len())
            .ok_or_else(|| {
                format!(
                    "CEF painted {}x{} into {} bytes",
                    size.0,
                    size.1,
                    bgra.len()
                )
            })?;
        let mut ring = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(slot) = (0..SLOTS)
            .map(|i| (ring.next + i) % SLOTS)
            .find(|&slot| !ring.busy[slot])
        else {
            return Ok(None);
        };
        let Ring { files, rgba, .. } = &mut *ring;
        rgba.resize(bytes, 0);
        straight_rgba(&bgra[..bytes], rgba);
        let slot_u8 = u8::try_from(slot).map_err(|_| "too many slots")?;
        let handle = handle(page, slot_u8);
        if files[slot].is_none() {
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(slot_file(dir, handle))
                .map_err(|e| format!("making a slot file: {e}"))?;
            files[slot] = Some(file);
        }
        let file = files[slot].as_ref().ok_or("the slot has no file")?;
        file.write_all_at(rgba, 0)
            .map_err(|e| format!("writing a slot file: {e}"))?;
        ring.busy[slot] = true;
        ring.next = slot + 1;
        Ok(Some(Painted {
            slot: slot_u8,
            handle,
            width: size.0,
            height: size.1,
        }))
    }

    /// Removes page `page`'s slot files from `dir`, once its browser has closed. The
    /// engine may still hold them open, which keeps their frames readable until it lets
    /// go.
    pub(crate) fn remove(&self, dir: &Path, page: u32) {
        let mut ring = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        for (slot, file) in ring.files.iter_mut().enumerate() {
            if file.take().is_some()
                && let Ok(slot) = u8::try_from(slot)
            {
                let _ = std::fs::remove_file(slot_file(dir, handle(page, slot)));
            }
        }
    }
}
