//! The store against a fake download: no test touches the network (R-TEST-004). The one
//! that does, `downloads_the_pinned_release_from_cisco`, needs the `openh264-download`
//! feature and `--ignored`, and CI runs neither.

use std::cell::RefCell;
use std::path::PathBuf;

use jstream_platform::media::{Codec, FrameSize, PixelFormat, Timebase};
use jstream_platform::{HardwarePreference, PlatformError, RateControl, VideoEncoderConfig};

use super::{Fetch, OPENH264_PIN, OpenH264Pin, OpenH264Store, sha256, wants_software};

/// "hello, hello, hello world\n", as `bzip2 -9` writes it.
const ARCHIVE: &[u8] = &[
    0x42, 0x5a, 0x68, 0x39, 0x31, 0x41, 0x59, 0x26, 0x53, 0x59, 0xcc, 0x2e, 0x03, 0xa1, 0x00, 0x00,
    0x06, 0x51, 0x80, 0x00, 0x10, 0x40, 0x04, 0x06, 0x44, 0x90, 0x80, 0x20, 0x00, 0x21, 0x90, 0x32,
    0x04, 0x00, 0xc2, 0xa8, 0x32, 0xd9, 0x41, 0x6e, 0x9c, 0x07, 0x9a, 0xf1, 0x77, 0x24, 0x53, 0x85,
    0x09, 0x0c, 0xc2, 0xe0, 0x3a, 0x10,
];
const LIBRARY: &[u8] = b"hello, hello, hello world\n";

/// A download that hands back fixed bytes and records what it was asked for.
struct FakeFetch {
    body: Result<Vec<u8>, PlatformError>,
    asked: RefCell<Vec<String>>,
}

impl FakeFetch {
    fn new(body: Result<Vec<u8>, PlatformError>) -> Self {
        Self {
            body,
            asked: RefCell::new(Vec::new()),
        }
    }
}

impl Fetch for FakeFetch {
    fn get(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, PlatformError> {
        assert_eq!(max_bytes, super::MAX_DOWNLOAD_BYTES);
        self.asked.borrow_mut().push(url.to_owned());
        self.body.clone()
    }
}

fn pin() -> OpenH264Pin {
    OpenH264Pin {
        version: (2, 6, 0),
        url: "https://ciscobinary.openh264.org/test.so.bz2",
        file_name: "libopenh264-test.so",
        archive_sha256: Some(sha256(ARCHIVE)),
        library_sha256: Some(sha256(LIBRARY)),
    }
}

/// A fresh folder for one test.
fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jstream-openh264-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn config(hardware: HardwarePreference) -> VideoEncoderConfig {
    VideoEncoderConfig {
        codec: Codec::H264,
        size: FrameSize::new(64, 32).expect("valid size"),
        input_format: PixelFormat::Nv12,
        frame_period: Timebase::new(1, 30).expect("valid period"),
        rate_control: RateControl::Cbr { kbps: 2500 },
        keyframe_interval: 60,
        hardware,
    }
}

#[test]
fn installs_checks_and_removes() {
    let dir = dir("install");
    let store = OpenH264Store::with_pin(dir.join("codecs/openh264"), pin());
    assert!(!store.is_installed());
    let fetch = FakeFetch::new(Ok(ARCHIVE.to_vec()));
    store.install(&fetch).expect("installs");
    assert_eq!(*fetch.asked.borrow(), [pin().url]);
    assert!(store.is_installed());
    assert_eq!(
        std::fs::read(store.library_path()).expect("stored"),
        LIBRARY
    );

    // Only the user can read it.
    use std::os::unix::fs::PermissionsExt;
    let mode =
        |p: &std::path::Path| std::fs::metadata(p).expect("exists").permissions().mode() & 0o777;
    assert_eq!(mode(&store.library_path()), 0o600);
    assert_eq!(mode(&dir.join("codecs/openh264")), 0o700);

    // A second install replaces it.
    store.install(&fetch).expect("installs again");

    store.remove().expect("removes");
    assert!(!store.is_installed());
    store.remove().expect("removing nothing succeeds");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_download_that_doesnt_match_the_pin_is_discarded() {
    let dir = dir("mismatch");
    let store = OpenH264Store::with_pin(dir.clone(), pin());
    let mut tampered = ARCHIVE.to_vec();
    tampered[20] ^= 1;
    let err = store
        .install(&FakeFetch::new(Ok(tampered)))
        .expect_err("refused");
    assert!(
        matches!(&err, PlatformError::Os { message, .. } if message.contains("discarded")),
        "{err:?}"
    );
    assert!(!store.is_installed());
    assert!(!dir.exists(), "nothing was written");

    // A file that matches the archive pin but unpacks to another library is refused too.
    let wrong_library = OpenH264Pin {
        library_sha256: Some([0; 32]),
        ..pin()
    };
    let store = OpenH264Store::with_pin(dir.clone(), wrong_library);
    assert!(
        store
            .install(&FakeFetch::new(Ok(ARCHIVE.to_vec())))
            .is_err()
    );
    assert!(!dir.exists(), "nothing was written");

    // A failed download is reported as it is.
    let store = OpenH264Store::with_pin(dir.clone(), pin());
    let failed = PlatformError::Os {
        call: "test",
        code: None,
        message: "offline".into(),
    };
    assert_eq!(
        store.install(&FakeFetch::new(Err(failed.clone()))),
        Err(failed)
    );
}

#[test]
fn nothing_downloads_without_a_recorded_hash() {
    let store = OpenH264Store::new(dir("unpinned"));
    let fetch = FakeFetch::new(Ok(ARCHIVE.to_vec()));
    if OPENH264_PIN.archive_sha256.is_none() || OPENH264_PIN.library_sha256.is_none() {
        assert!(matches!(
            store.install(&fetch),
            Err(PlatformError::Unsupported { .. })
        ));
        assert!(fetch.asked.borrow().is_empty(), "no request was made");
    }
    // The pinned address is Cisco's own server, over HTTPS.
    assert!(
        OPENH264_PIN
            .url
            .starts_with("https://ciscobinary.openh264.org/")
    );
}

#[test]
fn only_the_stored_library_is_loaded() {
    let dir = dir("load");
    let store = OpenH264Store::with_pin(dir.clone(), pin());
    let cfg = config(HardwarePreference::Avoid);
    // Not installed.
    assert!(matches!(
        store.video_encoder(&cfg),
        Err(PlatformError::MissingComponent { .. })
    ));
    // A file put there by anything but the download is refused before it is loaded.
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(store.library_path(), b"someone else's library").expect("write");
    let Err(err) = store.video_encoder(&cfg) else {
        panic!("a foreign library was loaded");
    };
    assert!(
        matches!(&err, PlatformError::MissingComponent { remedy, .. } if remedy.contains("not the library")),
        "{err:?}"
    );
    // The right bytes that aren't a shared library fail to load, and say so.
    std::fs::write(store.library_path(), LIBRARY).expect("write");
    let Err(err) = store.video_encoder(&cfg) else {
        panic!("not a library, yet it loaded");
    };
    assert!(
        matches!(&err, PlatformError::MissingComponent { remedy, .. } if remedy.contains("didn't load")),
        "{err:?}"
    );
    // A configuration OpenH264 can't take is refused before anything loads.
    let mut hevc = cfg;
    hevc.codec = Codec::Hevc;
    assert!(matches!(
        store.video_encoder(&hevc),
        Err(PlatformError::Unsupported { .. })
    ));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn software_serves_avoid_and_machines_without_hardware() {
    use HardwarePreference::{Avoid, Prefer, Require};
    assert!(wants_software(1, &config(Avoid)));
    assert!(wants_software(0, &config(Avoid)));
    assert!(wants_software(0, &config(Prefer)));
    assert!(!wants_software(1, &config(Prefer)));
    assert!(!wants_software(0, &config(Require)));
    let mut hevc = config(Avoid);
    hevc.codec = Codec::Hevc;
    assert!(!wants_software(0, &hevc));
}

#[test]
fn vaapi_codecs_route_software_requests_to_openh264() {
    use jstream_platform::Codecs;
    let dir = dir("codecs");
    let codecs =
        crate::VaapiCodecs::new().with_openh264(OpenH264Store::with_pin(dir.clone(), pin()));
    let listed = |c: &crate::VaapiCodecs| {
        c.list()
            .expect("lists")
            .iter()
            .any(|i| i.name == super::OPENH264_ENCODER_NAME && !i.hardware)
    };
    // Not downloaded: not listed, and a software request says what to download.
    assert!(!listed(&codecs));
    let Err(err) = codecs.video_encoder(&config(HardwarePreference::Avoid)) else {
        panic!("no OpenH264 is installed");
    };
    assert!(
        matches!(&err, PlatformError::MissingComponent { component, .. }
            if *component == super::OPENH264_COMPONENT),
        "{err:?}"
    );
    // Downloaded: listed as a software H.264 encoder.
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join(pin().file_name), LIBRARY).expect("write");
    assert!(listed(&codecs));
    let _ = std::fs::remove_dir_all(dir);
}

/// Downloads the pinned release from Cisco and checks it against the pin. While the pin
/// has no hashes, it fails with both SHA-256 values in its message, which is how a pin is
/// recorded. It touches the network, so it is behind the `openh264-download` feature and
/// `#[ignore]`d: run it by hand with `cargo test -p jstream-platform-linux --features
/// openh264-download -- --ignored downloads_the_pinned`.
#[cfg(feature = "openh264-download")]
#[test]
#[ignore = "downloads from Cisco's server"]
fn downloads_the_pinned_release_from_cisco() {
    let fetch = super::HttpsFetch::new().expect("TLS client");
    let archive = fetch
        .get(OPENH264_PIN.url, super::MAX_DOWNLOAD_BYTES)
        .expect("downloads");
    let library = super::bzip2::decompress(&archive, super::MAX_LIBRARY_BYTES).expect("unpacks");
    let (a, l) = (sha256(&archive), sha256(&library));
    let (Some(pa), Some(pl)) = (OPENH264_PIN.archive_sha256, OPENH264_PIN.library_sha256) else {
        panic!(
            "record the pin: archive_sha256 {}, library_sha256 {}",
            super::hex(&a),
            super::hex(&l)
        );
    };
    assert_eq!((a, l), (pa, pl), "Cisco's file no longer matches the pin");
    let dir = dir("download");
    let store = OpenH264Store::new(dir.clone());
    store.install(&fetch).expect("installs");
    store
        .video_encoder(&config(HardwarePreference::Avoid))
        .expect("opens an encoder on Cisco's library");
    let _ = std::fs::remove_dir_all(dir);
}
