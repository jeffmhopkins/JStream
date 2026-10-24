//! Cisco's OpenH264 binary: Linux's software H.264 encoder, downloaded from Cisco on the
//! user's request and loaded at run time (docs/adr/0061-source-types-and-decoding-scope-v2.md).
//!
//! JStream never ships, mirrors, builds or patches OpenH264. [`OpenH264Store::install`]
//! fetches the one pinned release ([`OPENH264_PIN`]) from Cisco's server, checks the
//! file's SHA-256 against the pin, unpacks it, checks the library's SHA-256, and only then
//! writes it into the data folder. Nothing calls `install` but a host acting on the
//! user's explicit request: nothing downloads at install or first run. A download whose
//! hash differs is discarded and reported, never written or loaded. The store loads only
//! the file it wrote, and checks its hash again each time; a copy from anywhere else is
//! never loaded. [`OpenH264Store::remove`] deletes it again.
//!
//! The encoder itself is [`OpenH264Encoder`], on hand-written declarations of the few
//! calls it makes (`ffi.rs`), so no crate links or builds OpenH264 and `deny.toml`'s
//! `openh264` bans stay (R-DEP-002).

mod bzip2;
mod encoder;
mod ffi;
mod https;

use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::Arc;

use jstream_platform::{PlatformError, VideoEncoder, VideoEncoderConfig};

use crate::dl::SystemLibrary;

pub use encoder::{OPENH264_ENCODER_NAME, OpenH264Encoder, openh264_encoder_info};
pub use https::HttpsFetch;

/// The component name in errors and in what the UI shows.
pub const OPENH264_COMPONENT: &str = "Cisco's OpenH264 (software H.264)";

/// The attribution the app shows wherever it offers the download and in its licence
/// information, beside a link to [`OPENH264_LICENSE_URL`] (ADR-0061 › Notices).
pub const OPENH264_NOTICE: &str = "OpenH264 Video Codec provided by Cisco Systems, Inc.";

/// Cisco's licence page for its binary, which the app links to wherever it offers the
/// download and in its licence information.
pub const OPENH264_LICENSE_URL: &str = "https://www.openh264.org/BINARY_LICENSE.txt";

/// The largest download accepted. Cisco's Linux library is about 1 MB compressed.
pub const MAX_DOWNLOAD_BYTES: usize = 16 << 20;

/// The largest unpacked library accepted.
const MAX_LIBRARY_BYTES: usize = 64 << 20;

/// One OpenH264 release, as JStream pins it: where it is on Cisco's server and the
/// SHA-256 of what is there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenH264Pin {
    /// The release, as `WelsGetCodecVersionEx` reports it. The declarations in
    /// `ffi.rs` match its headers, so the store refuses a library of any other version.
    pub version: (u32, u32, u32),
    /// The compressed library on Cisco's server.
    pub url: &'static str,
    /// The name the unpacked library is stored under.
    pub file_name: &'static str,
    /// The SHA-256 of the file at `url`. `None` until it is recorded, and then nothing
    /// downloads.
    pub archive_sha256: Option<[u8; 32]>,
    /// The SHA-256 of the unpacked library, checked before every load. `None` until it
    /// is recorded, as for `archive_sha256`.
    pub library_sha256: Option<[u8; 32]>,
}

/// The pinned release: OpenH264 2.6.0 for 64-bit x86 Linux. A newer release reaches
/// users through a JStream update that moves this pin (ADR-0061).
///
/// The two SHA-256 values are recorded by downloading the file from Cisco and hashing
/// it, which the `openh264-download` test does (feature `openh264-download`, never run
/// in CI). Until they are recorded, the store refuses to download.
pub const OPENH264_PIN: OpenH264Pin = OpenH264Pin {
    version: (2, 6, 0),
    url: "https://ciscobinary.openh264.org/libopenh264-2.6.0-linux64.8.so.bz2",
    file_name: "libopenh264-2.6.0-linux64.8.so",
    archive_sha256: None,
    library_sha256: None,
};

/// Fetches one file over the network: the download's seam, so the store's tests never
/// touch the network (R-TEST-004). [`HttpsFetch`] is the real one.
pub trait Fetch {
    /// The body at `url`, refusing more than `max_bytes`. Blocks; never call it on a
    /// real-time thread.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the request fails, the server answers anything but
    /// 200, or the body is larger than `max_bytes`.
    fn get(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, PlatformError>;
}

/// Where Cisco's library is kept and how it is checked: the folder JStream writes it to,
/// and the release it pins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenH264Store {
    dir: PathBuf,
    pin: OpenH264Pin,
}

impl OpenH264Store {
    /// The store in the data folder (`<data>/codecs/openh264`, ADR-0020), for the pinned
    /// release. `None` when the data folder can't be found.
    pub fn in_data_dir() -> Option<Self> {
        jstream_platform::paths::data_dir().map(|d| Self::new(d.join("codecs").join("openh264")))
    }

    /// The store in `dir`, for the pinned release.
    pub fn new(dir: PathBuf) -> Self {
        Self::with_pin(dir, OPENH264_PIN)
    }

    /// The store in `dir` for `pin`, for tests that pin their own file.
    pub fn with_pin(dir: PathBuf, pin: OpenH264Pin) -> Self {
        Self { dir, pin }
    }

    /// The release this store installs.
    pub fn pin(&self) -> &OpenH264Pin {
        &self.pin
    }

    /// Where the library is, once installed.
    pub fn library_path(&self) -> PathBuf {
        self.dir.join(self.pin.file_name)
    }

    /// Whether the library has been downloaded: the pin has a library hash to check it
    /// against, and the file is there. Its hash is checked when it is loaded, not here,
    /// so listing codecs doesn't read the whole file.
    pub fn is_installed(&self) -> bool {
        self.pin.library_sha256.is_some() && self.library_path().is_file()
    }

    /// Downloads the pinned release from Cisco through `fetch`, checks it and stores it.
    /// Call it only on the user's explicit request, after showing [`OPENH264_NOTICE`]
    /// and a link to [`OPENH264_LICENSE_URL`]. Blocks for the download.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the pin has no recorded SHA-256;
    /// [`PlatformError::Os`] when the download fails, when the file's or the library's
    /// SHA-256 isn't the pinned one (nothing is written), when it doesn't unpack, or
    /// when it can't be written.
    pub fn install(&self, fetch: &dyn Fetch) -> Result<(), PlatformError> {
        let (Some(archive_sha), Some(library_sha)) =
            (self.pin.archive_sha256, self.pin.library_sha256)
        else {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "downloading OpenH264 {}: this JStream has no recorded SHA-256 for \
                     Cisco's file yet (ADR-0061)",
                    version(&self.pin)
                ),
            });
        };
        let archive = fetch.get(self.pin.url, MAX_DOWNLOAD_BYTES)?;
        check_sha256("the OpenH264 download", &archive, &archive_sha)?;
        let library =
            bzip2::decompress(&archive, MAX_LIBRARY_BYTES).map_err(|e| PlatformError::Os {
                call: "unpacking the OpenH264 download",
                code: None,
                message: e.to_string(),
            })?;
        check_sha256("the unpacked OpenH264 library", &library, &library_sha)?;
        self.write(&library)
            .map_err(|e| io_error("storing the OpenH264 library", &e))
    }

    /// Deletes the library. Afterwards software H.264 reports not installed. Removing
    /// what isn't there succeeds.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the file exists but can't be deleted.
    pub fn remove(&self) -> Result<(), PlatformError> {
        match fs::remove_file(self.library_path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error("removing the OpenH264 library", &e)),
        }
    }

    /// Opens an encoder for `config` on the stored library.
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when the library isn't installed, has changed
    /// since it was stored, or doesn't load; otherwise as for opening the encoder.
    pub fn video_encoder(
        &self,
        config: &VideoEncoderConfig,
    ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
        encoder::params(config)?;
        let api = Arc::new(self.load()?);
        Ok(Box::new(OpenH264Encoder::open(api, config)?))
    }

    /// Loads the stored library after checking it is the one this store wrote.
    fn load(&self) -> Result<ffi::Api, PlatformError> {
        let path = self.library_path();
        let not_installed = || PlatformError::MissingComponent {
            component: OPENH264_COMPONENT,
            remedy: format!(
                "download it from Cisco through JStream (OpenH264 {}); JStream doesn't \
                 load a copy from anywhere else",
                version(&self.pin)
            ),
        };
        let Some(library_sha) = self.pin.library_sha256 else {
            return Err(not_installed());
        };
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_installed()),
            Err(e) => return Err(io_error("reading the OpenH264 library", &e)),
        };
        if sha256(&bytes) != library_sha {
            return Err(PlatformError::MissingComponent {
                component: OPENH264_COMPONENT,
                remedy: format!(
                    "{} is not the library JStream downloaded; remove it and download \
                     it again",
                    path.display()
                ),
            });
        }
        // SAFETY: the file's SHA-256 is the pinned one, so it is Cisco's library, whose
        // initialisers have no preconditions on the caller. (It could change between
        // the check and the load only by someone who can write the user's data folder.)
        let lib = unsafe { SystemLibrary::open_file(OPENH264_COMPONENT, &path) }?;
        ffi::Api::load(lib, self.pin.version)
    }

    /// Writes `library` to a temporary file only the user can read, then renames it into
    /// place, so a reader never sees half a file.
    fn write(&self, library: &[u8]) -> std::io::Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        let part = self.dir.join(format!("{}.part", self.pin.file_name));
        match fs::remove_file(&part) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&part)?;
        file.write_all(library)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&part, self.library_path())
    }
}

fn version(pin: &OpenH264Pin) -> String {
    let (a, b, c) = pin.version;
    format!("{a}.{b}.{c}")
}

fn io_error(call: &'static str, e: &std::io::Error) -> PlatformError {
    PlatformError::Os {
        call,
        code: e.raw_os_error().and_then(|c| u32::try_from(c).ok()),
        message: e.to_string(),
    }
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, data);
    let mut out = [0; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn check_sha256(what: &'static str, data: &[u8], pinned: &[u8; 32]) -> Result<(), PlatformError> {
    let found = sha256(data);
    if &found == pinned {
        Ok(())
    } else {
        Err(PlatformError::Os {
            call: "checking the OpenH264 download's SHA-256",
            code: None,
            message: format!(
                "{what} has SHA-256 {}, not the pinned {}; it was discarded",
                hex(&found),
                hex(pinned)
            ),
        })
    }
}

/// Whether `config` should go to the software encoder: H.264 with
/// [`HardwarePreference::Avoid`](jstream_platform::HardwarePreference::Avoid), or with
/// `Prefer` when there is no hardware encoder (ADR-0061: hardware first, OpenH264 last
/// in line).
pub fn wants_software(hardware_encoders: usize, config: &VideoEncoderConfig) -> bool {
    use jstream_platform::HardwarePreference;
    use jstream_platform::media::Codec;
    config.codec == Codec::H264
        && match config.hardware {
            HardwarePreference::Avoid => true,
            HardwarePreference::Prefer => hardware_encoders == 0,
            HardwarePreference::Require => false,
        }
}

#[cfg(test)]
mod tests;
