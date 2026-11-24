//! The camera the engine drew the shown 3D scene through (`scene.drawn_camera`, since
//! protocol 0.192), which the canvas outlines and hits the scene's items through. The
//! engine draws a scene's stored camera as its camera motion, follow camera, live aim
//! and the 3D group it rides with move it, so with any of them the stored camera is not
//! where the picture shows the items. The canvas reads the drawn camera about ten times
//! a second while it shows a 3D scene, one read at a time.

use jstream_control::protocol::{SceneCamera, SceneId};
use std::time::Duration;

use super::camera::Preview;

/// How often the canvas reads the drawn camera while it shows a 3D scene.
pub const READ_EVERY: Duration = Duration::from_millis(100);

/// One read of the drawn camera: the scene asked about, its stored camera when it was
/// asked, and what the engine answered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Read<'a> {
    /// The scene asked about.
    pub scene: &'a SceneId,
    /// The scene's stored camera as the canvas had it when it asked.
    pub stored: Option<SceneCamera>,
    /// The camera the engine drew it through last, `None` while no picture draws it.
    pub drawn: Option<SceneCamera>,
}

/// The camera the canvas sees `scene` through, whose stored camera is `stored`: an orbit,
/// dolly or flight's `preview` while it moves that scene; else what the engine last
/// `read` it drawn through, while that read was of this scene and its stored camera
/// hasn't changed since (an edit shows at once, not after the next read); else the
/// stored camera. A 2D scene, `stored` `None`, has none.
pub fn seen_through(
    preview: Option<&Preview>,
    read: Option<Read<'_>>,
    scene: Option<&SceneId>,
    stored: Option<SceneCamera>,
) -> Option<SceneCamera> {
    let through = Preview::camera_in(preview, scene, stored);
    if through != stored || stored.is_none() {
        return through;
    }
    match read {
        Some(Read {
            scene: read_scene,
            stored: then,
            drawn: Some(drawn),
        }) if scene == Some(read_scene) && then == stored => Some(drawn),
        _ => stored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{Canvas, Finite32};

    fn camera(x: f32) -> SceneCamera {
        let mut camera = SceneCamera::framing(Canvas::DEFAULT);
        camera.position.x = Finite32::new(x).expect("finite");
        camera
    }

    #[test]
    fn a_scene_is_seen_through_the_camera_the_engine_drew_it_through() {
        let a = SceneId::new("a");
        let stored = Some(camera(100.0));
        let read = Read {
            scene: &a,
            stored,
            drawn: Some(camera(900.0)),
        };
        assert_eq!(
            seen_through(None, Some(read), Some(&a), stored),
            Some(camera(900.0)),
            "camera motion moved it"
        );
        // Nothing read yet, or no picture draws it: its stored camera.
        assert_eq!(seen_through(None, None, Some(&a), stored), stored);
        let undrawn = Read {
            drawn: None,
            ..read
        };
        assert_eq!(seen_through(None, Some(undrawn), Some(&a), stored), stored);
    }

    #[test]
    fn a_drag_an_edit_another_scene_or_2d_leave_the_drawn_camera_aside() {
        let (a, b) = (SceneId::new("a"), SceneId::new("b"));
        let stored = Some(camera(100.0));
        let read = Read {
            scene: &a,
            stored,
            drawn: Some(camera(900.0)),
        };
        let orbit = Preview {
            scene: a.clone(),
            camera: camera(300.0),
        };
        assert_eq!(
            seen_through(Some(&orbit), Some(read), Some(&a), stored),
            Some(camera(300.0)),
            "the orbit the canvas drags"
        );
        let edited = Some(camera(200.0));
        assert_eq!(
            seen_through(None, Some(read), Some(&a), edited),
            edited,
            "an edit since the read shows at once"
        );
        assert_eq!(
            seen_through(None, Some(read), Some(&b), stored),
            stored,
            "a read of another scene"
        );
        assert_eq!(seen_through(None, Some(read), Some(&a), None), None, "2D");
    }
}
