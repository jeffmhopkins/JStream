//! The cameras each drawer last drew its scenes through (`scene.drawn_camera`, since
//! protocol 0.192): a 3D scene's camera as its camera motion, follow camera, live aim,
//! the 3D group it rides with and autofocus move it, which is where the picture shows
//! its items. An editor outlines and hits the scene's items through it, since the
//! scene's stored camera is not where they are drawn once anything moves it.
//!
//! The drawer notes each scene's camera as a frame draws its colour pass, and its meter
//! publishes the frame's cameras into its [`DrawerSlot`](crate::profile::DrawerSlot) as
//! one immutable [`DrawnCameras`], when they change, at most every [`PUBLISH_MIN`], and
//! again every [`REFRESH_EVERY`] while they don't, so a reader can tell a scene still
//! drawn from one no longer drawn. A steady frame allocates nothing; the set it replaces
//! goes where the caller says (the render thread's collector, R-RT-003).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use jstream_protocol::{SceneCamera, SceneId};

/// The shortest time between two publishes of changed cameras: a camera that moves
/// every frame publishes 20 times a second, not 60.
pub(crate) const PUBLISH_MIN: Duration = Duration::from_millis(50);

/// How often unchanged cameras are published again, so their time stays recent.
pub(crate) const REFRESH_EVERY: Duration = Duration::from_millis(250);

/// How long after its drawer published it a camera still counts as drawn.
pub(crate) const FRESH_FOR: Duration = Duration::from_secs(1);

/// The cameras one drawer's latest frame drew its scenes through, and when it said so.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrawnCameras {
    pub(crate) at: Instant,
    pub(crate) cameras: BTreeMap<SceneId, SceneCamera>,
}

/// A drawer's side: the cameras of the frame being drawn, and what it published last.
#[derive(Debug, Default)]
pub(crate) struct CameraRecorder {
    frame: BTreeMap<SceneId, SceneCamera>,
    /// The cameras last published and when, or `None` before the first publish.
    sent: Option<(Instant, BTreeMap<SceneId, SceneCamera>)>,
}

impl CameraRecorder {
    /// Notes that the frame being drawn drew `scene` through `camera`; a 2D scene's pass,
    /// `None`, notes nothing.
    pub(crate) fn drew(&mut self, scene: &SceneId, camera: Option<SceneCamera>) {
        let Some(camera) = camera else {
            return;
        };
        // The entry nearly always exists: clone the scene only to insert it.
        match self.frame.get_mut(scene) {
            Some(held) => *held = camera,
            None => {
                self.frame.insert(scene.clone(), camera);
            }
        }
    }

    /// Ends the frame drawn: publishes its cameras into `slot` when they changed and
    /// [`PUBLISH_MIN`] has passed since the last publish, or [`REFRESH_EVERY`] has, and
    /// hands the set it replaces to `retire`. Then starts the next frame empty.
    pub(crate) fn end_frame(
        &mut self,
        now: Instant,
        slot: Option<&ArcSwapOption<DrawnCameras>>,
        retire: impl FnOnce(Arc<DrawnCameras>),
    ) {
        let due = match &self.sent {
            None => !self.frame.is_empty(),
            Some((at, sent)) => {
                let since = now.saturating_duration_since(*at);
                since >= REFRESH_EVERY || (since >= PUBLISH_MIN && *sent != self.frame)
            }
        };
        if due {
            match &mut self.sent {
                Some((at, sent)) => {
                    *at = now;
                    sent.clone_from(&self.frame);
                }
                None => self.sent = Some((now, self.frame.clone())),
            }
            if let Some(slot) = slot {
                let published = Arc::new(DrawnCameras {
                    at: now,
                    cameras: self.frame.clone(),
                });
                if let Some(old) = slot.swap(Some(published)) {
                    retire(old);
                }
            }
        }
        self.frame.clear();
    }
}

/// The camera `scene` was drawn through last by any of `slots`, the most recently
/// published one, unless every drawer that drew it published longer than
/// [`FRESH_FOR`] before `now`.
pub(crate) fn latest<'a>(
    slots: impl IntoIterator<Item = &'a ArcSwapOption<DrawnCameras>>,
    scene: &SceneId,
    now: Instant,
) -> Option<SceneCamera> {
    let mut best: Option<(Instant, SceneCamera)> = None;
    for slot in slots {
        let Some(drawn) = slot.load_full() else {
            continue;
        };
        if now.saturating_duration_since(drawn.at) > FRESH_FOR {
            continue;
        }
        let Some(camera) = drawn.cameras.get(scene) else {
            continue;
        };
        if best.is_none_or(|(at, _)| drawn.at > at) {
            best = Some((drawn.at, *camera));
        }
    }
    best.map(|(_, camera)| camera)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_protocol::{Canvas as CameraCanvas, Finite32};

    fn camera(x: f32) -> SceneCamera {
        let mut camera = SceneCamera::framing(CameraCanvas::DEFAULT);
        camera.position.x = Finite32::new(x).expect("finite");
        camera
    }

    fn scene(id: &str) -> SceneId {
        SceneId::new(id)
    }

    #[test]
    fn a_frame_publishes_the_cameras_it_drew_its_scenes_through() {
        let slot = ArcSwapOption::empty();
        let mut recorder = CameraRecorder::default();
        let start = Instant::now();
        recorder.drew(&scene("a"), Some(camera(10.0)));
        recorder.drew(&scene("flat"), None);
        recorder.end_frame(start, Some(&slot), drop);
        assert_eq!(latest([&slot], &scene("a"), start), Some(camera(10.0)));
        assert_eq!(latest([&slot], &scene("flat"), start), None);
    }

    #[test]
    fn a_moving_camera_publishes_at_most_every_publish_min() {
        let slot = ArcSwapOption::empty();
        let mut recorder = CameraRecorder::default();
        let start = Instant::now();
        recorder.drew(&scene("a"), Some(camera(1.0)));
        recorder.end_frame(start, Some(&slot), drop);
        let mut retired = 0;
        recorder.drew(&scene("a"), Some(camera(2.0)));
        recorder.end_frame(start + PUBLISH_MIN / 2, Some(&slot), |_| retired += 1);
        assert_eq!(latest([&slot], &scene("a"), start), Some(camera(1.0)));
        assert_eq!(retired, 0);
        recorder.drew(&scene("a"), Some(camera(3.0)));
        recorder.end_frame(start + PUBLISH_MIN, Some(&slot), |_| retired += 1);
        assert_eq!(latest([&slot], &scene("a"), start), Some(camera(3.0)));
        assert_eq!(retired, 1, "the set it replaced goes to retire");
    }

    #[test]
    fn a_scene_no_longer_drawn_goes_and_a_stopped_drawer_goes_stale() {
        let slot = ArcSwapOption::empty();
        let mut recorder = CameraRecorder::default();
        let start = Instant::now();
        recorder.drew(&scene("a"), Some(camera(1.0)));
        recorder.end_frame(start, Some(&slot), drop);
        // An unchanged camera is published again, so it stays fresh while drawn.
        let mut at = start;
        for _ in 0..8 {
            at += REFRESH_EVERY;
            recorder.drew(&scene("a"), Some(camera(1.0)));
            recorder.end_frame(at, Some(&slot), drop);
        }
        assert_eq!(latest([&slot], &scene("a"), at), Some(camera(1.0)));
        // Once the drawer stops drawing it, it goes from the next publish.
        at += REFRESH_EVERY;
        recorder.end_frame(at, Some(&slot), drop);
        assert_eq!(latest([&slot], &scene("a"), at), None);
        // A drawer that stops altogether goes stale.
        recorder.drew(&scene("a"), Some(camera(1.0)));
        at += REFRESH_EVERY;
        recorder.end_frame(at, Some(&slot), drop);
        assert_eq!(
            latest([&slot], &scene("a"), at + FRESH_FOR + PUBLISH_MIN),
            None
        );
    }

    #[test]
    fn the_most_recent_drawer_wins() {
        let (program, preview) = (ArcSwapOption::empty(), ArcSwapOption::empty());
        let start = Instant::now();
        let mut first = CameraRecorder::default();
        first.drew(&scene("a"), Some(camera(1.0)));
        first.end_frame(start, Some(&program), drop);
        let mut second = CameraRecorder::default();
        second.drew(&scene("a"), Some(camera(2.0)));
        second.end_frame(start + PUBLISH_MIN, Some(&preview), drop);
        let now = start + PUBLISH_MIN;
        assert_eq!(
            latest([&program, &preview], &scene("a"), now),
            Some(camera(2.0))
        );
        assert_eq!(
            latest([&preview, &program], &scene("a"), now),
            Some(camera(2.0))
        );
    }
}
