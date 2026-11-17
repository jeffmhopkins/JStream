//! Media files on Linux (ADR-0072): the [`MediaDecoders`] backend, [`LinuxMediaDecoders`].
//!
//! Linux has no OS media stack, so the crate demuxes and decodes on its own, in the
//! order ADR-0072 sets: pure-Rust demuxers and decoders first, VA-API for video, and
//! the system's FFmpeg only for the gaps, once its two gates pass (it isn't built yet).
//!
//! What plays so far:
//! - **Audio files** through symphonia's readers and decoders ([`audio`]): WAV, FLAC,
//!   Ogg Vorbis and MP3, with PCM, FLAC, Vorbis and MP3 audio.
//!
//! A container this backend can't read, or a codec it can't decode, is
//! [`PlatformError::Unsupported`], naming it. AAC audio stays off until ADR-0072 (g)'s
//! decoder patent check says yes.
//!
//! Everything here is pure Rust and platform-independent, and tested everywhere.

pub(crate) mod audio;

use std::fs::File;
use std::path::Path;

use jstream_platform::media::{AudioBuffer, VideoFrame};
use jstream_platform::{MediaDecoder, MediaDecoderConfig, MediaDecoders, MediaInfo, PlatformError};

use audio::AudioFile;

/// What `NotFound` names.
const KIND: &str = "media file";

/// Local media files on Linux: the [`MediaDecoders`] backend (ADR-0072).
///
/// Creating it loads nothing; each open file reads and decodes on the caller's thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct LinuxMediaDecoders;

impl MediaDecoders for LinuxMediaDecoders {
    fn open(
        &self,
        path: &Path,
        config: &MediaDecoderConfig,
    ) -> Result<Box<dyn MediaDecoder>, PlatformError> {
        let file = File::open(path).map_err(|err| open_error(path, &err))?;
        if let Some(audio) = AudioFile::open(path, file, config.audio)? {
            return Ok(Box::new(AudioOnly::new(audio)));
        }
        Err(PlatformError::Unsupported {
            what: format!(
                "this media file's container on Linux ({}): WAV, FLAC, Ogg and MP3 play so far",
                path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("no extension")
            ),
        })
    }
}

/// The error for a file that didn't open.
fn open_error(path: &Path, err: &std::io::Error) -> PlatformError {
    match err.kind() {
        std::io::ErrorKind::NotFound => PlatformError::NotFound {
            kind: KIND,
            id: path.display().to_string(),
        },
        _ => PlatformError::Os {
            call: "open",
            code: err.raw_os_error().and_then(|c| u32::try_from(c).ok()),
            message: err.to_string(),
        },
    }
}

/// An audio file: a [`MediaDecoder`] with no video.
struct AudioOnly {
    file: AudioFile,
    info: MediaInfo,
}

impl AudioOnly {
    fn new(file: AudioFile) -> Self {
        let info = MediaInfo {
            video: None,
            audio: file.info(),
            duration: file.duration(),
        };
        Self { file, info }
    }
}

impl MediaDecoder for AudioOnly {
    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn next_video(&mut self) -> Result<Option<VideoFrame>, PlatformError> {
        Ok(None)
    }

    fn next_audio(&mut self) -> Result<Option<AudioBuffer>, PlatformError> {
        self.file.next()
    }

    fn seek(&mut self, to: jstream_platform::media::MediaTime) -> Result<(), PlatformError> {
        self.file.seek(to)
    }
}

#[cfg(test)]
mod tests;
