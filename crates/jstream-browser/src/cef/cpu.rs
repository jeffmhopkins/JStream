//! The helper on Linux (ADR-0095): started on one end of a socket pair, which is its
//! standard input, and its paints read as CPU frames from slot files in a private folder
//! made for it ([`crate::cpu_frames`]).
//!
//! **The folder** is `jstream-browser-<pid>-<random>` under `$XDG_RUNTIME_DIR` (the
//! user's own, in memory), else `/dev/shm`, else the temp folder. It is created fresh
//! with mode 0700, so it can't already exist and only this user can reach it, and is
//! removed with everything in it once the helper has stopped.
//!
//! **A paint** names its slot file; the engine reads the whole frame from it into a new
//! CPU frame before telling the helper the slot is free, so a frame it hands out is
//! never written again.

use std::collections::HashMap;
use std::fs::{DirBuilder, File};
use std::hash::{BuildHasher, RandomState};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, FileExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use jstream_media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Plane, VideoFrame,
    VideoFrameInfo,
};
use jstream_platform::GpuFrame;

use crate::BrowserError;
use crate::cpu_frames::{SLOTS, frame_bytes, slot_file};
use crate::link::{Frames, PageFrames, Paint};

/// The private folder one helper writes its paints into; removed when dropped.
pub(super) struct FramesDir(PathBuf);

impl FramesDir {
    /// Makes a new folder, only this user's, under the first of `$XDG_RUNTIME_DIR`,
    /// `/dev/shm` and the temp folder that exists.
    pub(super) fn create() -> Result<Self, BrowserError> {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute() && dir.is_dir())
            .or_else(|| Some(PathBuf::from("/dev/shm")).filter(|dir| dir.is_dir()))
            .unwrap_or_else(std::env::temp_dir);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = base.join(format!(
            "jstream-browser-{}-{:016x}",
            std::process::id(),
            RandomState::new().hash_one(nanos)
        ));
        // `create`, not `create_all`: it fails if the name is already taken.
        DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| start(format!("making {}: {e}", path.display())))?;
        Ok(Self(path))
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for FramesDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            tracing::warn!(%error, "the browser helper's frame folder could not be removed");
        }
    }
}

/// Starts `exe` with `dir` for its paints and the other end of a fresh socket pair as its
/// standard input. Returns the child and this end, as (from the helper, to the helper).
pub(super) fn spawn(
    exe: &Path,
    dir: &FramesDir,
) -> Result<(Child, UnixStream, UnixStream), BrowserError> {
    let (ours, theirs) =
        UnixStream::pair().map_err(|e| start(format!("the browser helper's socket: {e}")))?;
    let to_helper = ours
        .try_clone()
        .map_err(|e| start(format!("the browser helper's socket: {e}")))?;
    // The command, and with it this process's copy of the helper's end, is dropped at
    // the end of the statement, so the helper's end is only in the helper.
    let child = Command::new(exe)
        .arg(format!("--jstream-frames={}", dir.path().display()))
        .stdin(Stdio::from(OwnedFd::from(theirs)))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| start(format!("starting {}: {e}", exe.display())))?;
    Ok((child, ours, to_helper))
}

/// Reads one helper's paints from its folder.
pub(super) struct CpuFrames {
    dir: Arc<Path>,
}

impl CpuFrames {
    pub(super) fn new(dir: &Path) -> Self {
        Self { dir: dir.into() }
    }
}

impl Frames for CpuFrames {
    fn page(&self, size: FrameSize) -> Box<dyn PageFrames> {
        Box::new(CpuPageFrames {
            dir: Arc::clone(&self.dir),
            size,
            state: Mutex::new(PageState::default()),
        })
    }

    fn on_cpu(&self) -> bool {
        true
    }
}

struct CpuPageFrames {
    dir: Arc<Path>,
    /// The page's size: no paint is read that is larger.
    size: FrameSize,
    state: Mutex<PageState>,
}

#[derive(Default)]
struct PageState {
    /// The slot files opened so far, by the helper's handle.
    files: HashMap<u64, File>,
    latest: Option<Arc<VideoFrame>>,
    /// Whether a failed paint was logged since the last good one.
    warned: bool,
}

impl PageFrames for CpuPageFrames {
    /// Reads the paint's slot file into the page's latest frame. A paint that can't be
    /// read is skipped, keeping the last frame, and logged once until one is read again.
    fn paint(&self, paint: &Paint, pts: MediaTime) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match read(&self.dir, self.size, &mut state.files, paint, pts) {
            Ok(frame) => {
                state.latest = Some(Arc::new(frame));
                state.warned = false;
            }
            Err(reason) => {
                if !state.warned {
                    tracing::warn!(%reason, "a browser page's paint could not be read");
                    state.warned = true;
                }
            }
        }
    }

    fn latest(&self) -> Option<GpuFrame> {
        None
    }

    fn latest_cpu(&self) -> Option<Arc<VideoFrame>> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .latest
            .clone()
    }
}

/// One paint's frame, read from its slot file in `dir`, for a page of `page` size.
fn read(
    dir: &Path,
    page: FrameSize,
    files: &mut HashMap<u64, File>,
    paint: &Paint,
    pts: MediaTime,
) -> Result<VideoFrame, String> {
    if usize::from(paint.slot) >= SLOTS {
        return Err(format!("the helper named slot {}", paint.slot));
    }
    if paint.width > page.width() || paint.height > page.height() {
        return Err(format!(
            "the helper painted {}x{} for a {}x{} page",
            paint.width,
            paint.height,
            page.width(),
            page.height()
        ));
    }
    let bytes = frame_bytes(paint.width, paint.height)
        .ok_or_else(|| format!("the helper painted {}x{}", paint.width, paint.height))?;
    if !files.contains_key(&paint.handle) {
        // At most one file per slot: a page's handles are fixed, so more means trouble.
        if files.len() >= SLOTS {
            files.clear();
        }
        let path = slot_file(dir, paint.handle);
        let file = File::open(&path).map_err(|e| format!("opening its slot file: {e}"))?;
        files.insert(paint.handle, file);
    }
    let file = files
        .get(&paint.handle)
        .ok_or("the slot file went missing")?;
    let mut rgba = vec![0; bytes];
    file.read_exact_at(&mut rgba, 0)
        .map_err(|e| format!("reading its slot file: {e}"))?;
    let size = FrameSize::new(paint.width, paint.height).map_err(|e| e.to_string())?;
    VideoFrame::new(
        VideoFrameInfo {
            size,
            format: PixelFormat::Rgba8,
            // Chromium composites in sRGB; every code value is used.
            color: ColorInfo {
                space: ColorSpace::Bt709,
                range: ColorRange::Full,
            },
            pts,
        },
        vec![Plane::new(rgba, size.width() as usize * 4)],
    )
    .map_err(|e| e.to_string())
}

fn start(reason: String) -> BrowserError {
    BrowserError::Start { reason }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_frames::handle;

    #[test]
    fn the_folder_is_private_and_goes_with_the_helper() {
        use std::os::unix::fs::PermissionsExt;
        let dir = FramesDir::create().expect("a folder");
        let path = dir.path().to_path_buf();
        let mode = std::fs::metadata(&path)
            .expect("it exists")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        std::fs::write(path.join("slot-1"), b"x").expect("a slot file");
        drop(dir);
        assert!(!path.exists());
    }

    #[test]
    fn a_paint_is_read_from_the_slot_file_it_names() {
        let dir = FramesDir::create().expect("a folder");
        let frames = CpuFrames::new(dir.path());
        assert!(frames.on_cpu());
        let page = frames.page(FrameSize::new(2, 1).expect("size"));
        let pts = MediaTime::zero(jstream_media::Timebase::MILLIS);
        let paint = Paint {
            slot: 1,
            handle: handle(7, 1),
            width: 2,
            height: 1,
        };
        // A file that isn't there yet: the paint is skipped.
        page.paint(&paint, pts);
        assert!(page.latest_cpu().is_none());
        let rgba = [1, 2, 3, 4, 5, 6, 7, 8];
        std::fs::write(slot_file(dir.path(), paint.handle), rgba).expect("a slot file");
        page.paint(&paint, pts);
        let frame = page.latest_cpu().expect("a frame");
        assert_eq!(frame.planes()[0].data(), &rgba);
        assert_eq!(frame.info().format, PixelFormat::Rgba8);
        // A paint larger than its page, or than any page, keeps the last frame.
        page.paint(&Paint { width: 4, ..paint }, pts);
        page.paint(
            &Paint {
                width: 100_000,
                ..paint
            },
            pts,
        );
        assert_eq!(
            page.latest_cpu().expect("a frame").planes()[0].data(),
            &rgba
        );
        assert!(page.latest().is_none());
    }

    #[test]
    fn the_helper_gets_the_socket_as_its_standard_input() {
        use std::io::{Read, Write};
        let dir = FramesDir::create().expect("a folder");
        // A helper that copies what it reads on its standard input back into it.
        let script = dir.path().join("helper.sh");
        std::fs::write(&script, "#!/bin/sh\nexec cat <&0 >&0\n").expect("a script");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
                .expect("executable");
        }
        let (mut child, mut from, mut to) = spawn(&script, &dir).expect("started");
        to.write_all(b"ping").expect("written");
        let mut echoed = [0; 4];
        from.read_exact(&mut echoed).expect("echoed");
        assert_eq!(&echoed, b"ping");
        let _ = child.kill();
        let _ = child.wait();
    }
}
