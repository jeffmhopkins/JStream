//! [`Recorder`]: fMP4 to a temporary file, synced per fragment, renamed on finish.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use jstream_media::Packet;
use jstream_mux::fmp4::{Fmp4Config, Fmp4Muxer, Segment};

use super::RecordError;

/// What a recording has written so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecordStats {
    /// Bytes written to the file, init segment included.
    pub bytes: u64,
    /// Media segments (fragments) written and synced to disk.
    pub fragments: u64,
}

/// The temporary name a recording to `path` is written under: the same directory, with
/// `.part` appended to the file name (`show.mp4` → `show.mp4.part`). `None` when `path`
/// has no file name.
pub fn temp_path_for(path: &Path) -> Option<PathBuf> {
    let mut name = OsString::from(path.file_name()?);
    name.push(".part");
    Some(path.with_file_name(name))
}

/// Records one fMP4 file from encoded H.264 (Annex B) and AAC packets.
///
/// [`Recorder::create`] opens `<path>.part` (see [`temp_path_for`]) and writes the init
/// segment once the decoder configurations are known. Each fragment the muxer
/// completes (one per video keyframe, or per second of audio-only) is written and then
/// synced with `sync_data`, so a crash or power loss keeps every completed fragment and
/// loses at most the one being built. [`Recorder::finish`] writes the last fragment,
/// syncs, closes, and renames the file to `path`.
///
/// Nothing is ever overwritten: an existing file at `path` or at its `.part` name is
/// [`RecordError::Exists`], checked on create and enforced atomically by the final
/// move, which never replaces a file that appeared meanwhile. A
/// write or sync error (a full disk is [`RecordError::DiskFull`]) ends the recording:
/// later calls return [`RecordError::Ended`], and the `.part` file is kept, readable
/// up to its last complete fragment. Dropping a recorder without finishing it keeps
/// the `.part` file too, as a crash would.
///
/// The calls block on the disk, so a recorder runs on its output's own thread, which
/// is fed through a bounded queue that drops and counts packets under overload
/// (R-RT-004), as the RTMP client is.
#[derive(Debug)]
pub struct Recorder {
    path: PathBuf,
    sink: Sink<File>,
}

impl Recorder {
    /// Starts a recording to `path`, writing to its `.part` name until
    /// [`finish`](Self::finish).
    ///
    /// # Errors
    /// [`RecordError::NoFileName`], [`RecordError::Config`] for a configuration the
    /// muxer refuses, [`RecordError::Exists`] when `path` or its `.part` name exists,
    /// [`RecordError::Create`] when the file can't be created, and
    /// [`RecordError::DiskFull`] or [`RecordError::Write`] when the init segment can't
    /// be written. Only the last two leave a (`.part`) file behind.
    pub fn create(path: impl Into<PathBuf>, config: Fmp4Config) -> Result<Self, RecordError> {
        let path = path.into();
        let temp =
            temp_path_for(&path).ok_or_else(|| RecordError::NoFileName { path: path.clone() })?;
        let muxer = Fmp4Muxer::new(config).map_err(RecordError::Config)?;
        check_absent(&path)?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|source| match source.kind() {
                io::ErrorKind::AlreadyExists => RecordError::Exists { path: temp.clone() },
                _ => RecordError::Create {
                    path: temp.clone(),
                    source,
                },
            })?;
        let mut sink = Sink::new(file, muxer, temp);
        sink.start()?;
        Ok(Self { path, sink })
    }

    /// Records one packet, writing and syncing the fragment it completes, if any.
    ///
    /// # Errors
    /// [`RecordError::Packet`] when the muxer rejects the packet; nothing is written
    /// and the recording carries on. [`RecordError::DiskFull`] or
    /// [`RecordError::Write`] when writing fails, and [`RecordError::Ended`] after an
    /// earlier failure; both mean the recording is over.
    pub fn write_packet(&mut self, packet: &Packet) -> Result<(), RecordError> {
        self.sink.write_packet(packet)
    }

    /// Writes the last fragment, syncs, closes the file and renames it to the
    /// destination, which it returns.
    ///
    /// # Errors
    /// [`RecordError::Ended`] after an earlier failure, [`RecordError::Finish`] when the
    /// muxer can't build the last fragment, [`RecordError::DiskFull`] or
    /// [`RecordError::Write`], [`RecordError::Exists`] when a file appeared at the
    /// destination meanwhile, and [`RecordError::Rename`]. In each case the `.part`
    /// file is kept; after the last two it holds the whole recording.
    pub fn finish(self) -> Result<PathBuf, RecordError> {
        let Self { path, mut sink } = self;
        sink.finish()?;
        let temp = sink.path.clone();
        // Close before renaming: Windows can't rename a file that is open.
        drop(sink);
        move_no_replace(&temp, &path)?;
        Ok(path)
    }

    /// The destination the finished file is renamed to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The temporary `.part` file being written.
    pub fn temp_path(&self) -> &Path {
        &self.sink.path
    }

    /// What has been written so far.
    pub fn stats(&self) -> RecordStats {
        self.sink.stats
    }

    /// Whether an error ended the recording.
    pub fn is_ended(&self) -> bool {
        self.sink.ended
    }
}

/// [`RecordError::Exists`] when something is at `path`.
fn check_absent(path: &Path) -> Result<(), RecordError> {
    match path.try_exists() {
        Ok(false) => Ok(()),
        Ok(true) => Err(RecordError::Exists {
            path: path.to_path_buf(),
        }),
        Err(source) => Err(RecordError::Create {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Moves the finished file `from` to `to` without ever replacing a file at `to`.
///
/// `std::fs::rename` replaces an existing destination on every platform, so checking
/// first and renaming after races a file created in between (#355). Instead this hard
/// links `from` to `to`, which fails with `AlreadyExists` atomically when `to` exists
/// (`link` on Unix, `CreateHardLinkW` on Windows), and then removes `from`. That keeps
/// the crate free of `unsafe` and of a new platform dependency; the true no-replace
/// renames (`renameat2(RENAME_NOREPLACE)`, `MoveFileExW` without
/// `MOVEFILE_REPLACE_EXISTING`) need one.
///
/// A file system without hard links (FAT, exFAT) falls back to
/// [`reserve_then_rename`]. If removing `from` fails after the link, the recording is
/// already complete at `to`, so this only logs the stray `.part` name.
///
/// # Errors
/// [`RecordError::Exists`] when a file is at `to`, and [`RecordError::Rename`] when
/// neither way works. `from` is kept in both cases.
fn move_no_replace(from: &Path, to: &Path) -> Result<(), RecordError> {
    match fs::hard_link(from, to) {
        Ok(()) => {
            if let Err(err) = fs::remove_file(from) {
                tracing::warn!(
                    part = %from.display(),
                    path = %to.display(),
                    %err,
                    "recording finished, but its .part name could not be removed"
                );
            }
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Err(RecordError::Exists {
            path: to.to_path_buf(),
        }),
        Err(link_err) => {
            tracing::debug!(
                path = %to.display(),
                %link_err,
                "hard link failed, reserving the destination instead"
            );
            reserve_then_rename(from, to)
        }
    }
}

/// The fallback of [`move_no_replace`] for file systems without hard links: creates
/// `to` exclusively (`create_new`, atomic on every platform) as an empty placeholder,
/// then renames `from` over that placeholder. The rename only ever replaces the file
/// this call just created, so another file at `to` is never overwritten. A crash
/// between the two steps leaves an empty `to` beside the complete `.part` file. A
/// failed rename removes the placeholder.
///
/// # Errors
/// [`RecordError::Exists`] when a file is at `to`, and [`RecordError::Rename`] when
/// the placeholder can't be created or the rename fails. `from` is kept in both cases.
fn reserve_then_rename(from: &Path, to: &Path) -> Result<(), RecordError> {
    let rename_error = |source| RecordError::Rename {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        source,
    };
    match OpenOptions::new().write(true).create_new(true).open(to) {
        // Close the placeholder before renaming over it: Windows can't replace an open
        // file.
        Ok(placeholder) => drop(placeholder),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            return Err(RecordError::Exists {
                path: to.to_path_buf(),
            });
        }
        Err(err) => return Err(rename_error(err)),
    }
    fs::rename(from, to).map_err(|err| {
        let _ = fs::remove_file(to);
        rename_error(err)
    })
}

/// A destination that can be made durable. A file syncs its data; tests use a writer
/// that fails on demand.
trait Durable: Write {
    /// Pushes everything written so far to stable storage.
    fn sync(&mut self) -> io::Result<()>;
}

impl Durable for File {
    fn sync(&mut self) -> io::Result<()> {
        self.sync_data()
    }
}

/// The muxer and its output, and whether a failure ended them.
#[derive(Debug)]
struct Sink<W> {
    out: W,
    muxer: Fmp4Muxer,
    /// The path `out` writes to, for errors.
    path: PathBuf,
    stats: RecordStats,
    ended: bool,
}

impl<W: Durable> Sink<W> {
    fn new(out: W, muxer: Fmp4Muxer, path: PathBuf) -> Self {
        Self {
            out,
            muxer,
            path,
            stats: RecordStats::default(),
            ended: false,
        }
    }

    /// Writes the init segment if the decoder configurations are already known.
    fn start(&mut self) -> Result<(), RecordError> {
        match self.muxer.start() {
            Ok(init) => self.write(init.as_slice()),
            Err(err) => {
                self.ended = true;
                Err(RecordError::Finish(err))
            }
        }
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<(), RecordError> {
        if self.ended {
            return Err(RecordError::Ended);
        }
        // A rejected packet leaves the muxer unchanged, so the recording carries on.
        let segments = self.muxer.push(packet).map_err(RecordError::Packet)?;
        self.write(&segments)
    }

    fn finish(&mut self) -> Result<(), RecordError> {
        if self.ended {
            return Err(RecordError::Ended);
        }
        self.ended = true;
        let segments = self.muxer.finish().map_err(RecordError::Finish)?;
        self.write(&segments)?;
        // The init segment of a recording with no media was never synced.
        self.out
            .flush()
            .and_then(|()| self.out.sync())
            .map_err(|source| RecordError::io(self.path.clone(), source))
    }

    /// Writes `segments`, syncing after each media segment. A failure ends the sink:
    /// the muxer has already moved past these segments, so the file can't continue.
    fn write(&mut self, segments: &[Segment]) -> Result<(), RecordError> {
        for segment in segments {
            let result = self.out.write_all(segment.bytes()).and_then(|()| {
                if let Segment::Media { .. } = segment {
                    self.out.flush()?;
                    self.out.sync()?;
                }
                Ok(())
            });
            if let Err(source) = result {
                self.ended = true;
                return Err(RecordError::io(self.path.clone(), source));
            }
            self.stats.bytes += segment.bytes().len() as u64;
            if let Segment::Media { .. } = segment {
                self.stats.fragments += 1;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
