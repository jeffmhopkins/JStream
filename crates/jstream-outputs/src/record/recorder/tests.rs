use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jstream_media::{Codec, MediaTime, Packet, Timebase};
use jstream_mux::aac::AudioSpecificConfig;
use jstream_mux::fmp4::{AudioTrack, Fmp4Config, Fmp4Muxer, Fmp4Writer, Segment, VideoTrack};
use jstream_mux::h264::AvcDecoderConfig;

use super::{Durable, Sink, move_no_replace, reserve_then_rename, temp_path_for};
use crate::record::{RecordError, RecordStats, Recorder};

/// A Baseline-compatible High-profile SPS (1280×720) and its PPS, as `jstream-mux`'s
/// tests use.
const SPS: [u8; 25] = [
    0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBB, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00,
    0x10, 0x00, 0x00, 0x03, 0x03, 0xC0, 0xF1, 0x83, 0x19,
];
const PPS: [u8; 6] = [0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0];

/// A directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("jstream-record-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn config(avc: bool) -> Fmp4Config {
    Fmp4Config {
        video: Some(VideoTrack {
            width: 1280,
            height: 720,
            avc: avc
                .then(|| AvcDecoderConfig::new(vec![SPS.to_vec()], vec![PPS.to_vec()]).unwrap()),
        }),
        audio: Some(AudioTrack {
            config: AudioSpecificConfig::aac_lc(48_000, 2).unwrap(),
            bitrate_kbps: Some(128),
        }),
        origin: None,
    }
}

/// An IDR access unit carrying SPS and PPS in-band, as Media Foundation emits it.
fn idr() -> Vec<u8> {
    let mut au = Vec::new();
    for nalu in [&SPS[..], &PPS[..], &[0x65, 0x88, 0x84, 0x00, 0x01]] {
        au.extend_from_slice(&[0, 0, 0, 1]);
        au.extend_from_slice(nalu);
    }
    au
}

fn video(key: bool, frame: i64) -> Packet {
    let t = MediaTime::new(frame * 3_000, Timebase::MPEG_90K);
    let data = if key {
        idr()
    } else {
        vec![0, 0, 0, 1, 0x41, 0x9A, frame as u8]
    };
    Packet::new(Codec::H264, key, t, t, data).unwrap()
}

fn aac(frame: i64) -> Packet {
    let t = MediaTime::new(frame * 1_024, Timebase::per_second(48_000).unwrap());
    Packet::new(Codec::Aac, true, t, t, vec![0x21, frame as u8]).unwrap()
}

/// Two seconds of 30 fps video with a keyframe every second, and the AAC frames that
/// go with it, interleaved in time order.
fn packets() -> Vec<Packet> {
    let mut out = Vec::new();
    let mut a = 0;
    for v in 0..60 {
        out.push(video(v % 30 == 0, v));
        // AAC frames whose start is before the next video frame's.
        while a * 1_024 * 90_000 < (v + 1) * 3_000 * 48_000 {
            out.push(aac(a));
            a += 1;
        }
    }
    out
}

/// What `Fmp4Writer` produces for the same packets, to compare files with.
fn expected(config: Fmp4Config, packets: &[Packet]) -> Vec<u8> {
    let mut writer = Fmp4Writer::new(Vec::new(), config).unwrap();
    for p in packets {
        writer.write_packet(p).unwrap();
    }
    writer.finish().unwrap()
}

#[test]
fn temp_name_appends_part_in_the_same_directory() {
    assert_eq!(
        temp_path_for(Path::new("dir/show.mp4")),
        Some(PathBuf::from("dir/show.mp4.part"))
    );
    assert_eq!(temp_path_for(Path::new("..")), None);
}

#[test]
fn records_to_part_file_then_renames_on_finish() {
    let dir = TempDir::new("finish");
    let path = dir.join("show.mp4");
    let packets = packets();
    let mut recorder = Recorder::create(&path, config(true)).unwrap();
    assert_eq!(recorder.temp_path(), dir.join("show.mp4.part"));
    for p in &packets {
        recorder.write_packet(p).unwrap();
    }
    assert!(!path.exists());
    assert!(recorder.temp_path().exists());
    // Keyframes at 0, 1 s: the second closes the first fragment.
    assert_eq!(recorder.stats().fragments, 1);
    assert_eq!(recorder.finish().unwrap(), path);

    assert!(!dir.join("show.mp4.part").exists());
    assert_eq!(fs::read(&path).unwrap(), expected(config(true), &packets));
}

#[test]
fn avc_config_from_the_stream_delays_the_init_segment() {
    let dir = TempDir::new("inband");
    let path = dir.join("show.mp4");
    let packets = packets();
    let mut recorder = Recorder::create(&path, config(false)).unwrap();
    assert_eq!(recorder.stats(), RecordStats::default());
    for p in &packets {
        recorder.write_packet(p).unwrap();
    }
    recorder.finish().unwrap();
    assert_eq!(fs::read(&path).unwrap(), expected(config(false), &packets));
}

#[test]
fn part_file_holds_every_completed_fragment() {
    // What a crash leaves: the recorder is dropped without finishing.
    let dir = TempDir::new("crash");
    let path = dir.join("show.mp4");
    let packets = packets();
    let mut muxer = Fmp4Muxer::new(config(true)).unwrap();
    let mut on_disk: Vec<u8> = muxer.start().unwrap().unwrap().bytes().to_vec();

    let mut recorder = Recorder::create(&path, config(true)).unwrap();
    let temp = recorder.temp_path().to_path_buf();
    assert_eq!(fs::read(&temp).unwrap(), on_disk);
    for p in &packets {
        recorder.write_packet(p).unwrap();
        for segment in muxer.push(p).unwrap() {
            on_disk.extend_from_slice(segment.bytes());
        }
        // After every packet, the file is exactly the segments completed so far.
        assert_eq!(fs::read(&temp).unwrap(), on_disk);
    }
    assert_eq!(
        recorder.stats(),
        RecordStats {
            bytes: on_disk.len() as u64,
            fragments: 1,
        }
    );
    drop(recorder);
    assert!(!path.exists());
    assert_eq!(fs::read(&temp).unwrap(), on_disk);
}

#[test]
fn never_overwrites_the_destination_or_a_leftover_part_file() {
    let dir = TempDir::new("exists");
    let path = dir.join("show.mp4");
    fs::write(&path, b"keep").unwrap();
    let err = Recorder::create(&path, config(true)).unwrap_err();
    assert!(
        matches!(&err, RecordError::Exists { path: p } if *p == path),
        "{err}"
    );
    assert!(!dir.join("show.mp4.part").exists());

    fs::remove_file(&path).unwrap();
    let temp = dir.join("show.mp4.part");
    fs::write(&temp, b"crashed").unwrap();
    let err = Recorder::create(&path, config(true)).unwrap_err();
    assert!(
        matches!(&err, RecordError::Exists { path: p } if *p == temp),
        "{err}"
    );
    assert_eq!(fs::read(&temp).unwrap(), b"crashed");
}

#[test]
fn finish_refuses_a_destination_that_appeared_meanwhile() {
    let dir = TempDir::new("raced");
    let path = dir.join("show.mp4");
    let packets = packets();
    let mut recorder = Recorder::create(&path, config(true)).unwrap();
    for p in &packets {
        recorder.write_packet(p).unwrap();
    }
    fs::write(&path, b"keep").unwrap();
    let err = recorder.finish().unwrap_err();
    assert!(matches!(err, RecordError::Exists { .. }), "{err}");
    assert_eq!(fs::read(&path).unwrap(), b"keep");
    // The whole recording is kept under its temporary name.
    assert_eq!(
        fs::read(dir.join("show.mp4.part")).unwrap(),
        expected(config(true), &packets)
    );
}

// #355: a file created after any check and right before the final move is never
// replaced, by the hard-link move and by its fallback for file systems without links.
#[test]
fn the_final_move_never_replaces_a_destination_created_just_before_it() {
    type Move = fn(&Path, &Path) -> Result<(), RecordError>;
    let moves: [(&str, Move); 2] = [("link", move_no_replace), ("reserve", reserve_then_rename)];
    for (name, move_file) in moves {
        let dir = TempDir::new(&format!("no-replace-{name}"));
        let temp = dir.join("show.mp4.part");
        let path = dir.join("show.mp4");
        fs::write(&temp, b"recording").unwrap();
        fs::write(&path, b"keep").unwrap();
        let err = move_file(&temp, &path).unwrap_err();
        assert!(
            matches!(&err, RecordError::Exists { path: p } if *p == path),
            "{name}: {err}"
        );
        assert_eq!(fs::read(&path).unwrap(), b"keep", "{name}");
        assert_eq!(fs::read(&temp).unwrap(), b"recording", "{name}");

        fs::remove_file(&path).unwrap();
        move_file(&temp, &path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"recording", "{name}");
        assert!(!temp.exists(), "{name}");
    }
}

// #355, deterministically: a dangling symlink at the destination is invisible to an
// existence check (`try_exists` follows it and finds nothing), just like a file that
// appears right after the check, but it is still an entry a rename would replace.
// `finish` must refuse it, and so must the placeholder fallback.
#[cfg(unix)]
#[test]
fn finish_never_replaces_a_destination_an_existence_check_misses() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("dangling");
    let path = dir.join("show.mp4");
    let temp = dir.join("show.mp4.part");
    let packets = packets();
    let mut recorder = Recorder::create(&path, config(true)).unwrap();
    for p in &packets {
        recorder.write_packet(p).unwrap();
    }
    symlink(dir.join("nowhere"), &path).unwrap();
    assert!(!path.try_exists().unwrap());

    let err = recorder.finish().unwrap_err();
    assert!(
        matches!(&err, RecordError::Exists { path: p } if *p == path),
        "{err}"
    );
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&temp).unwrap(), expected(config(true), &packets));

    let err = reserve_then_rename(&temp, &path).unwrap_err();
    assert!(
        matches!(&err, RecordError::Exists { path: p } if *p == path),
        "{err}"
    );
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&temp).unwrap(), expected(config(true), &packets));
}

#[test]
fn a_path_without_a_file_name_is_refused() {
    let err = Recorder::create("..", config(true)).unwrap_err();
    assert!(matches!(err, RecordError::NoFileName { .. }), "{err}");
}

#[test]
fn a_missing_directory_is_a_create_error() {
    let dir = TempDir::new("nodir");
    let err = Recorder::create(dir.join("missing/show.mp4"), config(true)).unwrap_err();
    assert!(matches!(err, RecordError::Create { .. }), "{err}");
    assert!(err.ends_recording());
}

#[test]
fn a_rejected_packet_does_not_end_the_recording() {
    let dir = TempDir::new("rejected");
    let path = dir.join("show.mp4");
    let mut recorder = Recorder::create(&path, config(true)).unwrap();
    // The first video packet must be a keyframe.
    let err = recorder.write_packet(&video(false, 0)).unwrap_err();
    assert!(matches!(err, RecordError::Packet(_)), "{err}");
    assert!(!err.ends_recording());
    assert!(!recorder.is_ended());

    let packets = packets();
    for p in &packets {
        recorder.write_packet(p).unwrap();
    }
    recorder.finish().unwrap();
    assert_eq!(fs::read(&path).unwrap(), expected(config(true), &packets));
}

#[test]
fn finish_without_a_keyframe_keeps_the_part_file() {
    // Audio is buffered, but no keyframe ever gave the file its AVC configuration.
    let dir = TempDir::new("nokey");
    let path = dir.join("show.mp4");
    let mut recorder = Recorder::create(&path, config(false)).unwrap();
    recorder.write_packet(&aac(0)).unwrap();
    let temp = recorder.temp_path().to_path_buf();
    let err = recorder.finish().unwrap_err();
    assert!(matches!(err, RecordError::Finish(_)), "{err}");
    assert!(temp.exists());
    assert!(!path.exists());
}

/// A file whose disk fills after `room` bytes, or whose syncs fail.
#[derive(Default)]
struct FailingFile {
    data: Vec<u8>,
    room: Option<usize>,
    write_error: Option<io::ErrorKind>,
    fail_sync: bool,
    syncs: usize,
}

impl io::Write for FailingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let room = self.room.map_or(buf.len(), |r| r - self.data.len());
        if room == 0 {
            return Err(self
                .write_error
                .unwrap_or(io::ErrorKind::StorageFull)
                .into());
        }
        let n = buf.len().min(room);
        self.data.extend_from_slice(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Durable for FailingFile {
    fn sync(&mut self) -> io::Result<()> {
        self.syncs += 1;
        if self.fail_sync {
            Err(io::Error::other("sync failed"))
        } else {
            Ok(())
        }
    }
}

fn failing_sink(file: FailingFile) -> Sink<FailingFile> {
    let mut sink = Sink::new(
        file,
        Fmp4Muxer::new(config(true)).unwrap(),
        PathBuf::from("show.mp4.part"),
    );
    sink.start().unwrap();
    sink
}

/// The init segment's length, and the end of the first fragment.
fn segment_ends() -> (usize, usize) {
    let mut muxer = Fmp4Muxer::new(config(true)).unwrap();
    let init = muxer.start().unwrap().unwrap().bytes().len();
    let first = packets()
        .iter()
        .flat_map(|p| muxer.push(p).unwrap())
        .find_map(|s| match s {
            Segment::Media { bytes, .. } => Some(bytes.len()),
            Segment::Init(_) => None,
        })
        .unwrap();
    (init, init + first)
}

#[test]
fn full_disk_ends_the_recording_with_a_typed_error() {
    let (_, first_end) = segment_ends();
    // Room for the init segment, the first fragment, and part of the next.
    let mut sink = failing_sink(FailingFile {
        room: Some(first_end + 10),
        ..FailingFile::default()
    });
    let mut failed = None;
    for p in packets() {
        if let Err(err) = sink.write_packet(&p) {
            failed = Some(err);
            break;
        }
    }
    // The second fragment is only written by `finish`, which fills the disk.
    let err = match failed {
        Some(err) => err,
        None => sink.finish().unwrap_err(),
    };
    assert!(
        matches!(&err, RecordError::DiskFull { path, .. } if path == Path::new("show.mp4.part")),
        "{err}"
    );
    assert!(err.ends_recording());
    assert!(err.to_string().contains("show.mp4.part"), "{err}");
    assert!(matches!(
        sink.write_packet(&aac(1_000)),
        Err(RecordError::Ended)
    ));
    assert!(matches!(sink.finish(), Err(RecordError::Ended)));
    // The first fragment was synced before the disk filled.
    assert_eq!(sink.stats.fragments, 1);
    assert_eq!(sink.stats.bytes, first_end as u64);
}

#[test]
fn other_write_errors_end_the_recording() {
    let (init_end, _) = segment_ends();
    let mut sink = failing_sink(FailingFile {
        room: Some(init_end),
        write_error: Some(io::ErrorKind::PermissionDenied),
        ..FailingFile::default()
    });
    let err = packets()
        .iter()
        .find_map(|p| sink.write_packet(p).err())
        .unwrap();
    assert!(matches!(err, RecordError::Write { .. }), "{err}");
    assert!(sink.ended);
}

#[test]
fn every_fragment_is_synced_and_a_failed_sync_ends_the_recording() {
    let mut sink = failing_sink(FailingFile::default());
    for p in packets() {
        sink.write_packet(&p).unwrap();
    }
    assert_eq!(sink.out.syncs, 1);
    sink.finish().unwrap();
    // The last fragment, and the final sync.
    assert_eq!(sink.out.syncs, 3);

    let mut sink = failing_sink(FailingFile {
        fail_sync: true,
        ..FailingFile::default()
    });
    let err = packets()
        .iter()
        .find_map(|p| sink.write_packet(p).err())
        .unwrap();
    assert!(matches!(err, RecordError::Write { .. }), "{err}");
    // The fragment's bytes went out, but it wasn't durable, so it isn't counted.
    assert_eq!(sink.stats.fragments, 0);
}
