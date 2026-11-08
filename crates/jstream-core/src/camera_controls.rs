//! Camera controls (#1196): each `webcam` source's `camera_controls` setting, set on its
//! device as it changes.
//!
//! The control side diffs what the drawn collection asks (live values included, so a
//! slider being dragged applies as it moves) against what it last sent, and hands the
//! changes to the `jstream-camera-controls` thread, which calls the backend's
//! [`VideoDevices::set_control`]. A set can take a while (on Windows, a device that
//! isn't open is activated for it), so it never runs under the engine lock; the thread
//! applies the latest value of each control and drops the ones a newer change
//! replaced. The backend applies a set to a running stream without reopening it, so a
//! change never restarts a capture.
//!
//! Two sources of one device that set the same control differently: the one listed
//! later wins. A control left out of the setting keeps what the device has. A device
//! plugged in again gets the settings when they next change, or when the engine starts.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use jstream_graph::State;
use jstream_platform::{CameraControlId, CameraControlValue, DeviceId, VideoDevices};
use jstream_protocol::SourceKind;
use jstream_sources::{CameraControlSetting, webcam_controls};

/// A device as a `webcam`'s `device` setting names it: `None` for the first one listed.
type DeviceKey = Option<String>;

/// One control to set.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Apply {
    device: DeviceKey,
    control: CameraControlId,
    value: CameraControlValue,
}

/// What the control side has sent, and the thread it sends to.
#[derive(Default)]
pub(crate) struct CameraControls {
    /// The settings sent for each device, by control name: unknown names included, so
    /// they aren't looked at again.
    sent: BTreeMap<DeviceKey, BTreeMap<String, CameraControlSetting>>,
    /// The thread's queue, started with the first change once the backend is known.
    worker: Option<Sender<Vec<Apply>>>,
    devices: Option<Arc<dyn VideoDevices>>,
}

impl CameraControls {
    /// Sets the backend the controls go to, and sends what `state` asks to it: what was
    /// recorded before the backend arrived was never applied.
    pub(crate) fn set_devices(&mut self, devices: Arc<dyn VideoDevices>, state: &State) {
        self.devices = Some(devices);
        self.worker = None;
        self.sent.clear();
        self.sync(state);
    }

    /// Sends the controls `state`'s `webcam` sources ask that changed since the last
    /// call.
    pub(crate) fn sync(&mut self, state: &State) {
        let Some(devices) = &self.devices else {
            return;
        };
        let wanted = wanted(state);
        let changes = changes(&self.sent, &wanted);
        if changes.is_empty() {
            self.sent = wanted;
            return;
        }
        // With no thread yet, or one that is gone (it panicked), start one for these.
        let unsent = match &self.worker {
            Some(worker) => worker.send(changes).err().map(|mpsc::SendError(back)| back),
            None => Some(changes),
        };
        if let Some(changes) = unsent {
            // This runs under the collection lock, where nothing is logged: a thread
            // that can't start leaves `sent` as it was, so the next change tries again.
            let Some(fresh) = spawn(Arc::clone(devices)) else {
                self.worker = None;
                return;
            };
            let _ = fresh.send(changes);
            self.worker = Some(fresh);
        }
        self.sent = wanted;
    }
}

/// The controls `state`'s `webcam` sources set, by device; a later source's entry wins.
fn wanted(state: &State) -> BTreeMap<DeviceKey, BTreeMap<String, CameraControlSetting>> {
    let mut out: BTreeMap<DeviceKey, BTreeMap<String, CameraControlSetting>> = BTreeMap::new();
    for source in state.sources() {
        let SourceKind::Builtin { kind } = source.kind() else {
            continue;
        };
        let Some((device, controls)) = webcam_controls(kind, source.settings()) else {
            continue;
        };
        if !controls.is_empty() {
            out.entry(device).or_default().extend(controls);
        }
    }
    out
}

/// The sets that take `sent` to `wanted`: each control whose setting is new or
/// different, by a name the platform knows. A control dropped from the setting is left
/// as the device has it.
fn changes(
    sent: &BTreeMap<DeviceKey, BTreeMap<String, CameraControlSetting>>,
    wanted: &BTreeMap<DeviceKey, BTreeMap<String, CameraControlSetting>>,
) -> Vec<Apply> {
    let mut out = Vec::new();
    for (device, controls) in wanted {
        let before = sent.get(device);
        for (name, setting) in controls {
            if before.and_then(|b| b.get(name)) == Some(setting) {
                continue;
            }
            let Some(control) = CameraControlId::from_name(name) else {
                continue;
            };
            out.push(Apply {
                device: device.clone(),
                control,
                value: match *setting {
                    CameraControlSetting::Manual(v) => CameraControlValue::Manual(v),
                    CameraControlSetting::Auto => CameraControlValue::Auto,
                },
            });
        }
    }
    out
}

/// Starts the thread, `None` when the OS refuses one.
fn spawn(devices: Arc<dyn VideoDevices>) -> Option<Sender<Vec<Apply>>> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("jstream-camera-controls".into())
        .spawn(move || run(&*devices, &rx))
        .ok()?;
    Some(tx)
}

/// Applies each batch, after taking every batch already queued, so a control dragged
/// through many values is set to the last of them.
fn run(devices: &dyn VideoDevices, rx: &Receiver<Vec<Apply>>) {
    while let Ok(first) = rx.recv() {
        let mut latest: BTreeMap<(DeviceKey, CameraControlId), CameraControlValue> =
            BTreeMap::new();
        for apply in std::iter::once(first).chain(rx.try_iter()).flatten() {
            latest.insert((apply.device, apply.control), apply.value);
        }
        let mut first_listed = None;
        for ((device, control), value) in latest {
            let id = match device {
                Some(id) => DeviceId::new(id),
                None => {
                    let first = first_listed.get_or_insert_with(|| {
                        devices
                            .devices()
                            .ok()
                            .and_then(|list| list.into_iter().next())
                            .map(|d| d.id)
                    });
                    let Some(id) = first.clone() else {
                        tracing::warn!(
                            control = control.name(),
                            "no video device to set the camera control on"
                        );
                        continue;
                    };
                    id
                }
            };
            if let Err(err) = devices.set_control(&id, control, value) {
                tracing::warn!(device = %id, control = control.name(), ?value, error = %err, "setting a camera control");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, CameraControlSetting)]) -> BTreeMap<String, CameraControlSetting> {
        entries.iter().map(|(n, s)| ((*n).to_owned(), *s)).collect()
    }

    #[test]
    fn only_new_or_changed_known_controls_are_sent() {
        let webcam: DeviceKey = Some("cam".into());
        let sent = BTreeMap::from([(
            webcam.clone(),
            map(&[
                ("exposure", CameraControlSetting::Manual(-6)),
                ("focus", CameraControlSetting::Auto),
            ]),
        )]);
        let wanted = BTreeMap::from([
            (
                webcam.clone(),
                map(&[
                    ("exposure", CameraControlSetting::Manual(-5)),
                    ("focus", CameraControlSetting::Auto),
                    ("shutter_angle", CameraControlSetting::Manual(1)),
                ]),
            ),
            (None, map(&[("white_balance", CameraControlSetting::Auto)])),
        ]);
        assert_eq!(
            changes(&sent, &wanted),
            [
                Apply {
                    device: None,
                    control: CameraControlId::WhiteBalance,
                    value: CameraControlValue::Auto,
                },
                Apply {
                    device: webcam,
                    control: CameraControlId::Exposure,
                    value: CameraControlValue::Manual(-5),
                },
            ]
        );
    }

    #[test]
    fn a_batch_sets_the_latest_value_of_each_control_and_the_first_device_for_none() {
        use jstream_platform::fake::{FAKE_WEBCAM, FakeClock, FakeVideoDevices};
        use jstream_platform::media::Timebase;
        let fake = FakeVideoDevices::new(FakeClock::new(Timebase::MILLIS));
        let (tx, rx) = mpsc::channel();
        let set = |device: DeviceKey, value| Apply {
            device,
            control: CameraControlId::WhiteBalance,
            value: CameraControlValue::Manual(value),
        };
        tx.send(vec![set(None, 3000), set(None, 5000)]).unwrap();
        tx.send(vec![Apply {
            device: Some(FAKE_WEBCAM.into()),
            control: CameraControlId::Brightness,
            value: CameraControlValue::Manual(10),
        }])
        .unwrap();
        drop(tx);
        run(&fake, &rx);
        let controls = fake.controls(&DeviceId::new(FAKE_WEBCAM)).unwrap();
        let get = |id| controls.iter().find(|c| c.id == id).unwrap();
        assert_eq!(get(CameraControlId::WhiteBalance).value, 5000);
        assert_eq!(get(CameraControlId::WhiteBalance).auto, Some(false));
        assert_eq!(get(CameraControlId::Brightness).value, 10);
    }
}
