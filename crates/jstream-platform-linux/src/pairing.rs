//! Pairing a video capture device with the microphone on the same hardware: a webcam's
//! own microphone, or a capture card's HDMI audio, as
//! [`VideoDeviceInfo::audio`](jstream_platform::VideoDeviceInfo::audio) reports it.
//!
//! The kernel exposes both halves under the same device in sysfs. A USB webcam is one
//! USB device with a video interface, which `uvcvideo` serves as `/dev/videoN`, and an
//! audio interface, which `snd-usb-audio` serves as ALSA sound card `K`. So the video
//! node's device (its USB interface's parent, or the device itself when it isn't a USB
//! interface) is the root, and every sound card whose device sits at or under that root
//! belongs to it ([`sound_cards_for`]). An audio backend then names its input on that
//! card ([`PairedAudio::card_inputs`]): PipeWire by the card index on its ALSA nodes,
//! ALSA by the card's id in its PCM names. A capture card whose audio is a separate PCI
//! function, rather than the same device, isn't paired.

// The sysfs walk is used by V4L2 (64-bit Linux) and by the Unix-only sysfs tests.
#[cfg(any(all(target_os = "linux", target_pointer_width = "64"), all(test, unix)))]
use std::path::{Path, PathBuf};

use jstream_platform::DeviceId;

/// One ALSA sound card, as sysfs lists it under `/sys/class/sound/cardK`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoundCard {
    /// Its index `K`, as in `hw:K` and PipeWire's `api.alsa.pcm.card`.
    pub index: u32,
    /// Its id (`/sys/class/sound/cardK/id`), as in `hw:CARD=<id>`.
    pub id: String,
}

/// An audio backend's input devices by the sound card each records from, read once
/// for a whole device listing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CardInputs {
    /// Inputs by card index, in the backend's order of preference.
    by_index: Vec<(u32, DeviceId)>,
    /// Inputs by card id, in the backend's order of preference.
    by_id: Vec<(String, DeviceId)>,
}

impl CardInputs {
    /// Inputs keyed by card index, best first.
    pub(crate) fn by_index(by_index: Vec<(u32, DeviceId)>) -> Self {
        Self {
            by_index,
            by_id: Vec::new(),
        }
    }

    /// Inputs keyed by card id, best first.
    pub(crate) fn by_id(by_id: Vec<(String, DeviceId)>) -> Self {
        Self {
            by_index: Vec::new(),
            by_id,
        }
    }

    /// The input to pair with a device whose sound cards are `cards`: the backend's
    /// preferred input on the first of them that has one.
    pub fn input_for(&self, cards: &[SoundCard]) -> Option<DeviceId> {
        cards.iter().find_map(|card| {
            let by_index = self.by_index.iter().find(|(k, _)| *k == card.index);
            let by_id = || self.by_id.iter().find(|(id, _)| *id == card.id);
            by_index
                .map(|(_, d)| d)
                .or_else(|| by_id().map(|(_, d)| d))
                .cloned()
        })
    }
}

/// An [`AudioIo`](jstream_platform::AudioIo) that can say which of its inputs records
/// from which sound card, so [`V4l2Devices`](crate::V4l2Devices) can pair a video device
/// with its microphone. [`PipeWireAudio`](crate::PipeWireAudio) and
/// [`AlsaAudio`](crate::AlsaAudio) implement it.
#[cfg(target_os = "linux")]
pub trait PairedAudio: jstream_platform::AudioIo + std::fmt::Debug {
    /// The backend's inputs by sound card, as they are now.
    ///
    /// # Errors
    /// As for the backend's `devices`.
    fn card_inputs(&self) -> Result<CardInputs, jstream_platform::PlatformError>;
}

/// The card id in an ALSA PCM name's `CARD=` argument (`sysdefault:CARD=C920`,
/// `hw:CARD=C920,DEV=0`), if it has one.
pub(crate) fn alsa_card_id(pcm: &str) -> Option<&str> {
    let (_, args) = pcm.split_once(':')?;
    args.split(',')
        .find_map(|arg| arg.strip_prefix("CARD="))
        .filter(|id| !id.is_empty())
}

/// How good an ALSA PCM is as a card's paired input, lower first: `sysdefault` (the
/// card's own default, converting as needed), `plughw`, `dsnoop` (shared), then `hw`
/// (exclusive and unconverted), then anything else on the card.
fn alsa_rank(pcm: &str) -> u8 {
    match pcm.split_once(':').map(|(kind, _)| kind) {
        Some("sysdefault") => 0,
        Some("plughw") => 1,
        Some("dsnoop") => 2,
        Some("hw") => 3,
        _ => 4,
    }
}

/// ALSA's input PCMs by card id, best first for each card.
pub(crate) fn alsa_card_inputs<'a>(inputs: impl IntoIterator<Item = &'a str>) -> CardInputs {
    let mut found: Vec<(String, DeviceId)> = inputs
        .into_iter()
        .filter_map(|pcm| Some((alsa_card_id(pcm)?.to_owned(), DeviceId::new(pcm))))
        .collect();
    found.sort_by_key(|(_, d)| alsa_rank(d.as_str()));
    CardInputs::by_id(found)
}

/// The sound cards on the same hardware as video node `node` (`video0`), with the
/// sysfs tree at `sys` (`/sys`), in index order. Empty when the node or the cards can't
/// be read.
#[cfg(any(all(target_os = "linux", target_pointer_width = "64"), all(test, unix)))]
pub(crate) fn sound_cards_for(sys: &Path, node: &str) -> Vec<SoundCard> {
    let Some(root) = hardware_root(&sys.join("class/video4linux").join(node).join("device")) else {
        return Vec::new();
    };
    let Ok(dir) = std::fs::read_dir(sys.join("class/sound")) else {
        return Vec::new();
    };
    let mut cards: Vec<SoundCard> = dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let index: u32 = name.to_str()?.strip_prefix("card")?.parse().ok()?;
            let device = std::fs::canonicalize(e.path().join("device")).ok()?;
            if !device.starts_with(&root) {
                return None;
            }
            let id = std::fs::read_to_string(e.path().join("id")).ok()?;
            Some(SoundCard {
                index,
                id: id.trim().to_owned(),
            })
        })
        .collect();
    cards.sort_by_key(|c| c.index);
    cards
}

/// The device a video node's `device` link names, or its parent when that is a USB
/// interface (a directory with `bInterfaceNumber`): the USB device, which holds the
/// audio interface too.
#[cfg(any(all(target_os = "linux", target_pointer_width = "64"), all(test, unix)))]
fn hardware_root(device: &Path) -> Option<PathBuf> {
    let device = std::fs::canonicalize(device).ok()?;
    if device.join("bInterfaceNumber").is_file() {
        device.parent().map(Path::to_path_buf)
    } else {
        Some(device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(index: u32, id: &str) -> SoundCard {
        SoundCard {
            index,
            id: id.into(),
        }
    }

    #[test]
    fn alsa_names_give_their_card_id() {
        assert_eq!(alsa_card_id("sysdefault:CARD=C920"), Some("C920"));
        assert_eq!(alsa_card_id("hw:CARD=C920,DEV=0"), Some("C920"));
        assert_eq!(alsa_card_id("hw:DEV=0,CARD=PCH"), Some("PCH"));
        assert_eq!(alsa_card_id("default"), None);
        assert_eq!(alsa_card_id("pulse"), None);
        assert_eq!(alsa_card_id("hw:CARD=,DEV=0"), None);
    }

    /// ALSA prefers the card's `sysdefault`, then `plughw`, then `hw`, whatever order
    /// the hints came in, and leaves out PCMs on no card.
    #[test]
    fn alsa_pairs_the_cards_best_input() {
        let inputs = alsa_card_inputs([
            "default",
            "hw:CARD=C920,DEV=0",
            "plughw:CARD=C920,DEV=0",
            "sysdefault:CARD=PCH",
            "sysdefault:CARD=C920",
        ]);
        assert_eq!(
            inputs.input_for(&[card(2, "C920")]),
            Some(DeviceId::new("sysdefault:CARD=C920"))
        );
        let without_sysdefault = alsa_card_inputs(["hw:CARD=C920,DEV=0", "plughw:CARD=C920,DEV=0"]);
        assert_eq!(
            without_sysdefault.input_for(&[card(2, "C920")]),
            Some(DeviceId::new("plughw:CARD=C920,DEV=0"))
        );
        assert_eq!(inputs.input_for(&[card(3, "Other")]), None);
        assert_eq!(inputs.input_for(&[]), None);
    }

    /// PipeWire's inputs match by index; the first of a device's cards that has an
    /// input wins.
    #[test]
    fn inputs_match_by_card_index() {
        let inputs = CardInputs::by_index(vec![
            (
                1,
                DeviceId::new("alsa_input.pci-0000_00_1f.3.analog-stereo"),
            ),
            (
                2,
                DeviceId::new("alsa_input.usb-046d_C920-02.analog-stereo"),
            ),
        ]);
        assert_eq!(
            inputs.input_for(&[card(2, "C920")]),
            Some(DeviceId::new("alsa_input.usb-046d_C920-02.analog-stereo"))
        );
        assert_eq!(
            inputs.input_for(&[card(5, "X"), card(1, "PCH")]),
            Some(DeviceId::new("alsa_input.pci-0000_00_1f.3.analog-stereo"))
        );
        assert_eq!(inputs.input_for(&[card(5, "X")]), None);
    }

    /// The sysfs tests, which lay a tree out with symlinks: Unix only, since the module
    /// also builds for tests on other targets.
    #[cfg(unix)]
    mod sysfs {
        use super::*;

        /// A sysfs tree in a temporary directory, laid out as the kernel lays one out.
        struct Sys(PathBuf);

        impl Sys {
            fn new(name: &str) -> Self {
                let root = std::env::temp_dir()
                    .join(format!("jstream-pairing-{name}-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&root);
                std::fs::create_dir_all(&root).expect("temp dir");
                Self(root)
            }

            fn dir(&self, rel: &str) -> PathBuf {
                let p = self.0.join(rel);
                std::fs::create_dir_all(&p).expect("dir");
                p
            }

            /// `devices/<rel>`, a USB interface when `interface`.
            fn device(&self, rel: &str, interface: bool) -> PathBuf {
                let p = self.dir(&format!("devices/{rel}"));
                if interface {
                    std::fs::write(p.join("bInterfaceNumber"), "00\n").expect("write");
                }
                p
            }

            fn link(&self, class_entry: &str, target: &Path) {
                let entry = self.dir(class_entry);
                std::os::unix::fs::symlink(target, entry.join("device")).expect("symlink");
            }

            fn card(&self, index: u32, id: &str, device: &Path) {
                let entry = format!("class/sound/card{index}");
                self.link(&entry, device);
                std::fs::write(self.0.join(entry).join("id"), format!("{id}\n")).expect("write");
            }
        }

        impl Drop for Sys {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        /// A USB webcam's video and audio interfaces share their USB device, so its card
        /// pairs and the onboard card doesn't; a node with no card on its device pairs
        /// nothing, and a missing node reads as no cards.
        #[test]
        fn a_webcams_sound_card_is_found_under_its_usb_device() {
            let sys = Sys::new("usb");
            let video = sys.device("pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0", true);
            let audio = sys.device("pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.2", true);
            let other_usb = sys.device("pci0000:00/0000:00:14.0/usb1/1-3/1-3:1.0", true);
            let onboard = sys.device("pci0000:00/0000:00:1f.3", false);
            sys.link("class/video4linux/video0", &video);
            sys.link("class/video4linux/video2", &other_usb);
            sys.card(0, "PCH", &onboard);
            sys.card(2, "C920", &audio);

            assert_eq!(sound_cards_for(&sys.0, "video0"), vec![card(2, "C920")]);
            assert_eq!(sound_cards_for(&sys.0, "video2"), Vec::new());
            assert_eq!(sound_cards_for(&sys.0, "video9"), Vec::new());
        }

        /// A PCI capture card whose sound card is on the same PCI function pairs with it;
        /// one on another function of the same slot doesn't.
        #[test]
        fn a_capture_cards_sound_card_on_the_same_device_pairs() {
            let sys = Sys::new("pci");
            let card_dev = sys.device("pci0000:00/0000:00:1c.0/0000:03:00.0", false);
            let sibling = sys.device("pci0000:00/0000:00:1c.0/0000:03:00.1", false);
            sys.link("class/video4linux/video0", &card_dev);
            sys.card(1, "HDMI", &card_dev);
            sys.card(3, "Sibling", &sibling);

            assert_eq!(sound_cards_for(&sys.0, "video0"), vec![card(1, "HDMI")]);
        }
    }
}
