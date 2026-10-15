//! Pairing a video device with the audio input endpoint of the same hardware.
//!
//! Windows groups the functions of one physical device (a webcam's camera and microphone,
//! a capture card's video and HDMI audio) under one container ID. A video device and an
//! audio endpoint with the same container ID are the same hardware. Built-in devices all
//! share the computer's own container, so that one pairs nothing: a laptop's camera and
//! its microphone array are not one device.
//!
//! Platform-independent, so it is tested on every platform.

use jstream_platform::DeviceId;

/// The container ID of devices built into the computer (`{00000000-0000-0000-FFFF-
/// FFFFFFFFFFFF}`), which says nothing about which of them belong together.
const COMPUTER_CONTAINER: u128 = 0x0000_0000_0000_0000_ffff_ffff_ffff_ffff;

/// The first of `inputs` (audio input endpoints and their container IDs, in the audio
/// backend's order) in the same container as the video device, if any.
pub(crate) fn paired(
    video_container: Option<u128>,
    inputs: &[(DeviceId, Option<u128>)],
) -> Option<DeviceId> {
    let container = video_container.filter(|&c| c != 0 && c != COMPUTER_CONTAINER)?;
    inputs
        .iter()
        .find(|(_, c)| *c == Some(container))
        .map(|(id, _)| id.clone())
}

#[cfg(test)]
mod tests {
    use jstream_platform::DeviceId;

    use super::{COMPUTER_CONTAINER, paired};

    fn inputs() -> Vec<(DeviceId, Option<u128>)> {
        vec![
            (DeviceId::new("mic-array"), Some(COMPUTER_CONTAINER)),
            (DeviceId::new("usb-headset"), Some(7)),
            (DeviceId::new("no-container"), None),
            (DeviceId::new("webcam-mic"), Some(42)),
            (DeviceId::new("webcam-mic-2"), Some(42)),
        ]
    }

    #[test]
    fn pairs_the_first_endpoint_in_the_same_container() {
        assert_eq!(
            paired(Some(42), &inputs()),
            Some(DeviceId::new("webcam-mic"))
        );
    }

    #[test]
    fn built_in_and_unknown_containers_pair_nothing() {
        assert_eq!(paired(Some(COMPUTER_CONTAINER), &inputs()), None);
        assert_eq!(paired(Some(0), &inputs()), None);
        assert_eq!(paired(None, &inputs()), None);
        assert_eq!(paired(Some(99), &inputs()), None);
    }
}
