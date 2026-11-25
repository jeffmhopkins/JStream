//! Fly mode, a 3D scene's look-around mode inside camera mode: W A S D move the camera
//! as a game's fly camera does, Q and E roll it left and right, Space raises it and
//! Shift (or Z) lowers it, as in a game's creative flight, R levels it, and the mouse turns it where it stands. Its speed comes from the size of what the scene holds, so a small room and
//! a wide set both take about five seconds to cross.
//!
//! This module is the arithmetic: which key moves which way, how fast, and where a step
//! of held keys puts the camera. The canvas (`mod.rs`) holds the keys, runs the frames
//! and commits the flight as one undo step once the camera rests, as it commits a wheel
//! turn.
//!
//! Docs: crates/jstream-ui/README.md#canvas, docs/adr/0094-scene-cameras.md

use crate::canvas::camera::{self, Shape};
use jstream_control::protocol::{Canvas, ItemGeometry, SceneCamera, SolidBounds, Vec3};

/// A way the held keys move the camera, or Shift's faster flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    /// Along the line of sight (W, ↑).
    Forward,
    /// Back along it (S, ↓).
    Back,
    /// To the camera's left (A, ←).
    Left,
    /// To its right (D, →).
    Right,
    /// Straight up in the scene (Space, Page Up).
    Up,
    /// Straight down (Z, Page Down).
    Down,
    /// Straight down too, from Shift, which is also Shift+wheel's modifier: kept apart
    /// so the canvas can let it go when Shift turns out to be a modifier.
    Sink,
    /// Rolls it left, its picture turning clockwise, at [`ROLL_DEG_PER_S`] (Q).
    RollLeft,
    /// Rolls it right (E).
    RollRight,
}

impl Move {
    /// The move a key makes, by its physical position (`KeyboardEvent.code`), so the keys
    /// sit where a game's do on any layout. `None` for any other key.
    pub fn of_code(code: &str) -> Option<Move> {
        Some(match code {
            "KeyW" | "ArrowUp" => Move::Forward,
            "KeyS" | "ArrowDown" => Move::Back,
            "KeyA" | "ArrowLeft" => Move::Left,
            "KeyD" | "ArrowRight" => Move::Right,
            "Space" | "PageUp" => Move::Up,
            "KeyZ" | "PageDown" => Move::Down,
            "ShiftLeft" | "ShiftRight" => Move::Sink,
            "KeyQ" => Move::RollLeft,
            "KeyE" => Move::RollRight,
            _ => return None,
        })
    }

    fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// The keys held down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys(u16);

impl Keys {
    /// Holds `key` down.
    pub fn press(&mut self, key: Move) {
        self.0 |= key.bit();
    }

    /// Lets `key` go.
    pub fn release(&mut self, key: Move) {
        self.0 &= !key.bit();
    }

    /// Whether `key` is held.
    pub fn held(self, key: Move) -> bool {
        self.0 & key.bit() != 0
    }

    /// Whether any key is held.
    pub fn moving(self) -> bool {
        self.0 != 0
    }
}

/// Degrees a second Q and E roll the camera about its line of sight.
pub const ROLL_DEG_PER_S: f32 = 45.0;
/// Seconds the camera takes to fly across the scene's extent at the speed it starts at.
pub const CROSS_SECONDS: f32 = 5.0;
/// The slowest a scene's own speed is, in scene pixels a second, so a scene of one tiny
/// item still moves.
pub const MIN_SPEED: f32 = 40.0;
/// How much one wheel pixel scales the speed with Shift+wheel in fly mode:
/// `exp(−px × SPEED_PER_WHEEL_PX)`,
/// so the wheel away speeds up.
pub const SPEED_PER_WHEEL_PX: f32 = 0.002;
/// The least and most the wheel scales the scene's own speed by.
pub const SPEED_RANGE: (f32, f32) = (1.0 / 16.0, 16.0);
/// Degrees the camera turns per pixel the mouse moves, in fly mode: half an orbit's, as a
/// mouse that moves freely covers more ground than a drag.
pub const LOOK_DEG_PER_PX: f32 = camera::ORBIT_DEG_PER_PX * 0.5;
/// The longest step one frame takes, in seconds, so a stalled frame never jumps the
/// camera.
pub const MAX_STEP_S: f32 = 0.1;

/// The size of what a 3D scene holds, in scene pixels: the diagonal of the box around its
/// items' corners as the scene places them (a card's four, a model's card pushed out by
/// its depth both ways along the card's normal). When the scene shows 3D models only they
/// count, so a backdrop or a sky far behind a room doesn't set the room's pace. `None`
/// when nothing is placed.
pub fn extent<'a>(items: impl Iterator<Item = (&'a ItemGeometry, &'a Shape)>) -> Option<f32> {
    let mut all = Bounds::default();
    let mut models = Bounds::default();
    for (geometry, shape) in items {
        let corners = geometry.corners_in_scene();
        let depth = match shape {
            Shape::Model(bounds) => model_depth(geometry, *bounds),
            _ => 0.0,
        };
        let normal = normal(&corners).map(|v| v * depth * 0.5);
        for corner in corners {
            for side in [-1.0, 1.0] {
                let at = [0, 1, 2].map(|i| (corner[i] + normal[i] * side) as f32);
                all.add(at);
                if matches!(shape, Shape::Model(_)) {
                    models.add(at);
                }
            }
        }
    }
    models.diagonal().or_else(|| all.diagonal())
}

/// How deep a model placed in `geometry`'s box is, in scene pixels: its `bounds`' depth
/// at the scale their width takes to fill the box, or their height where they have no
/// width.
fn model_depth(geometry: &ItemGeometry, bounds: SolidBounds) -> f64 {
    let size = [0, 1, 2].map(|i| f64::from(bounds.max[i]) - f64::from(bounds.min[i]));
    let scale = if size[0] > 0.0 {
        f64::from(geometry.size.width.get()) / size[0]
    } else if size[1] > 0.0 {
        f64::from(geometry.size.height.get()) / size[1]
    } else {
        0.0
    };
    let depth = size[2] * scale;
    if depth.is_finite() {
        depth.max(0.0)
    } else {
        0.0
    }
}

/// The unit normal of a card from its four corners in order, or zero when it has none.
fn normal(corners: &[[f64; 3]; 4]) -> [f64; 3] {
    let a = [0, 1, 2].map(|i| corners[1][i] - corners[0][i]);
    let b = [0, 1, 2].map(|i| corners[3][i] - corners[0][i]);
    let n = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let length = n.iter().map(|v| v * v).sum::<f64>().sqrt();
    if length > 0.0 && length.is_finite() {
        n.map(|v| v / length)
    } else {
        [0.0; 3]
    }
}

/// The box around the points added to it.
#[derive(Default)]
struct Bounds(Option<([f32; 3], [f32; 3])>);

impl Bounds {
    fn add(&mut self, at: [f32; 3]) {
        if !at.iter().all(|v| v.is_finite()) {
            return;
        }
        self.0 = Some(match self.0 {
            None => (at, at),
            Some((min, max)) => (
                [0, 1, 2].map(|i| min[i].min(at[i])),
                [0, 1, 2].map(|i| max[i].max(at[i])),
            ),
        });
    }

    fn diagonal(&self) -> Option<f32> {
        let (min, max) = self.0?;
        let d = [0, 1, 2].map(|i| max[i] - min[i]);
        let length = d.iter().map(|v| v * v).sum::<f32>().sqrt();
        (length.is_finite() && length > 0.0).then_some(length)
    }
}

/// The speed the camera flies at, in scene pixels a second, for a scene of `extent`
/// ([`extent`]) or, with nothing placed, one the size of its `canvas`: across it in
/// [`CROSS_SECONDS`], never under [`MIN_SPEED`].
pub fn base_speed(extent: Option<f32>, canvas: Canvas) -> f32 {
    let extent = extent.unwrap_or_else(|| (canvas.width as f32).hypot(canvas.height as f32));
    (extent / CROSS_SECONDS).max(MIN_SPEED)
}

/// The wheel's scale `scale` turned by `delta` wheel pixels (the browser's `deltaY`):
/// away (up) flies faster, toward slower, within [`SPEED_RANGE`].
pub fn rescale(scale: f32, delta: f32) -> f32 {
    // A turn past the whole range only reaches its end.
    let turn = (-delta * SPEED_PER_WHEEL_PX).clamp(-8.0, 8.0);
    let scaled = scale * turn.exp();
    if scaled.is_finite() {
        scaled.clamp(SPEED_RANGE.0, SPEED_RANGE.1)
    } else {
        scale
    }
}

/// `start` flown for `seconds` at `speed` scene pixels a second by the `keys` held:
/// forward and back along its line of sight, left and right along the picture's right,
/// up and down straight along the scene's up (−y), whatever the camera's tilt. Its target
/// moves with it, so it keeps looking the same way at the same distance, and its field of
/// view and roll stay. Opposite keys cancel, and a diagonal is no faster than a straight
/// line. The roll keys roll it about its line of sight at [`ROLL_DEG_PER_S`], its
/// `roll_deg` kept within ±180°. A step is no longer than [`MAX_STEP_S`]; the wheel's
/// scale is in `speed`.
pub fn step(
    start: SceneCamera,
    canvas: Canvas,
    keys: Keys,
    speed: f32,
    seconds: f32,
) -> SceneCamera {
    let seconds = seconds.clamp(0.0, MAX_STEP_S);
    let moved = travel(start, canvas, keys, speed * seconds);
    // `roll_deg` turns the picture clockwise, which banks the camera left.
    let roll = f32::from(u8::from(keys.held(Move::RollLeft)))
        - f32::from(u8::from(keys.held(Move::RollRight)));
    if roll == 0.0 {
        return moved;
    }
    let degrees =
        (moved.roll_deg.get() + roll * ROLL_DEG_PER_S * seconds + 180.0).rem_euclid(360.0) - 180.0;
    SceneCamera {
        roll_deg: camera::finite(degrees, moved.roll_deg),
        ..moved
    }
}

/// `start` moved `distance` scene pixels the way the held move keys point.
fn travel(start: SceneCamera, canvas: Canvas, keys: Keys, distance: f32) -> SceneCamera {
    let Ok(view) = start.view(canvas) else {
        return start;
    };
    let [right, _, forward] = view.axes();
    let up = [0.0, -1.0, 0.0];
    let axis = |plus: Move, minus: Move| {
        f64::from(u8::from(keys.held(plus))) - f64::from(u8::from(keys.held(minus)))
    };
    let down = keys.held(Move::Down) || keys.held(Move::Sink);
    let amounts = [
        (forward, axis(Move::Forward, Move::Back)),
        (right, axis(Move::Right, Move::Left)),
        (
            up,
            f64::from(u8::from(keys.held(Move::Up))) - f64::from(u8::from(down)),
        ),
    ];
    let mut way = [0.0_f64; 3];
    for (direction, amount) in amounts {
        for i in 0..3 {
            way[i] += direction[i] * amount;
        }
    }
    let length = way.iter().map(|v| v * v).sum::<f64>().sqrt();
    if length <= 0.0 || !length.is_finite() {
        return start;
    }
    let distance = f64::from(distance);
    let shift = way.map(|v| (v / length * distance) as f32);
    let moved = |v: Vec3| {
        Vec3::new(
            v.x.get() + shift[0],
            v.y.get() + shift[1],
            v.z.get() + shift[2],
        )
    };
    match (moved(start.position), moved(start.target)) {
        (Some(position), Some(target)) => SceneCamera {
            position,
            target,
            ..start
        },
        _ => start,
    }
}

/// `start` levelled: its roll back to 0, so the scene's up is up in the picture again.
/// Where it is and what it looks at stay.
pub fn level(start: SceneCamera) -> SceneCamera {
    SceneCamera {
        roll_deg: jstream_control::protocol::Finite32::ZERO,
        ..start
    }
}

/// `start` turned where it stands by a mouse move of `dx`, `dy` pixels, as a head turns
/// ([`camera::look`]) at [`LOOK_DEG_PER_PX`].
pub fn look(start: SceneCamera, dx: f32, dy: f32) -> SceneCamera {
    let scale = LOOK_DEG_PER_PX / camera::ORBIT_DEG_PER_PX;
    camera::look(start, dx * scale, dy * scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{Size, SolidBounds, Vec2};

    const CANVAS: Canvas = Canvas {
        width: 1920,
        height: 1080,
    };

    fn get(v: Vec3) -> [f32; 3] {
        [v.x.get(), v.y.get(), v.z.get()]
    }

    fn held(moves: &[Move]) -> Keys {
        let mut keys = Keys::default();
        for m in moves {
            keys.press(*m);
        }
        keys
    }

    fn item(x: f32, y: f32, depth: f32, w: f32, h: f32) -> ItemGeometry {
        let mut g = ItemGeometry::try_new(
            Vec2::new(x, y).expect("finite"),
            Size::try_new(w, h).expect("positive"),
        )
        .expect("geometry");
        g.depth_px = jstream_control::protocol::Finite32::new(depth).expect("finite");
        g
    }

    #[test]
    fn keys_map_by_position() {
        assert_eq!(Move::of_code("KeyW"), Some(Move::Forward));
        assert_eq!(Move::of_code("KeyQ"), Some(Move::RollLeft));
        assert_eq!(Move::of_code("KeyE"), Some(Move::RollRight));
        assert_eq!(Move::of_code("Space"), Some(Move::Up));
        assert_eq!(Move::of_code("ShiftLeft"), Some(Move::Sink));
        assert_eq!(Move::of_code("KeyZ"), Some(Move::Down));
        assert_eq!(Move::of_code("KeyK"), None);
        let mut keys = Keys::default();
        assert!(!keys.moving());
        keys.press(Move::Left);
        keys.press(Move::Down);
        assert!(keys.moving());
        keys.release(Move::Left);
        assert!(keys.held(Move::Down) && !keys.held(Move::Left));
    }

    #[test]
    fn forward_moves_along_the_line_of_sight_and_keeps_the_target_distance() {
        let start = SceneCamera::framing(CANVAS);
        let moved = step(start, CANVAS, held(&[Move::Forward]), 100.0, 0.05);
        let (p0, p1) = (get(start.position), get(moved.position));
        // The framing camera looks along −z, toward the canvas.
        assert!((p0[2] - p1[2] - 5.0).abs() < 1e-3, "{p0:?} → {p1:?}");
        assert!((p1[0] - p0[0]).abs() < 1e-3 && (p1[1] - p0[1]).abs() < 1e-3);
        assert!((moved.target_distance() - start.target_distance()).abs() < 1e-2);
        assert_eq!(moved.fov_deg, start.fov_deg);
    }

    #[test]
    fn up_is_the_scenes_up_and_right_is_the_pictures_right() {
        let start = SceneCamera::framing(CANVAS);
        let up = step(start, CANVAS, held(&[Move::Up]), 100.0, 0.05);
        assert!(get(up.position)[1] < get(start.position)[1], "up is −y");
        for down in [Move::Down, Move::Sink] {
            let sunk = step(start, CANVAS, held(&[down]), 100.0, 0.05);
            assert!(get(sunk.position)[1] > get(start.position)[1]);
        }
        let both = step(start, CANVAS, held(&[Move::Down, Move::Sink]), 100.0, 0.05);
        assert!((get(both.position)[1] - get(start.position)[1] - 5.0).abs() < 1e-3);
        let right = step(start, CANVAS, held(&[Move::Right]), 100.0, 0.05);
        assert!(get(right.position)[0] > get(start.position)[0]);
    }

    #[test]
    fn q_and_e_roll_the_camera_about_its_line_of_sight() {
        let start = SceneCamera::framing(CANVAS);
        let right = step(start, CANVAS, held(&[Move::RollRight]), 100.0, 0.1);
        assert_eq!(right.position, start.position);
        assert_eq!(right.target, start.target);
        assert!(
            (right.roll_deg.get() + 4.5).abs() < 1e-4,
            "{}",
            right.roll_deg.get()
        );
        // Rolled right, the camera's right points down the scene (+y).
        let [axis, ..] = right.view(CANVAS).expect("a valid camera").axes();
        assert!(axis[1] > 0.0, "{axis:?}");
        let left = step(start, CANVAS, held(&[Move::RollLeft]), 100.0, 0.1);
        assert!((left.roll_deg.get() - 4.5).abs() < 1e-4);
        assert_eq!(
            step(
                start,
                CANVAS,
                held(&[Move::RollLeft, Move::RollRight]),
                100.0,
                0.1
            ),
            start
        );
        // Past 180° it wraps round.
        let mut tipped = start;
        tipped.roll_deg = jstream_control::protocol::Finite32::new(178.0).expect("finite");
        let wrapped = step(tipped, CANVAS, held(&[Move::RollLeft]), 100.0, 0.1);
        assert!((wrapped.roll_deg.get() + 177.5).abs() < 1e-3);
    }

    #[test]
    fn opposite_keys_cancel_and_diagonals_are_no_faster() {
        let start = SceneCamera::framing(CANVAS);
        assert_eq!(
            step(start, CANVAS, held(&[Move::Left, Move::Right]), 100.0, 0.05),
            start
        );
        let diagonal = step(
            start,
            CANVAS,
            held(&[Move::Forward, Move::Right]),
            100.0,
            0.05,
        );
        let p0 = get(start.position);
        let p1 = get(diagonal.position);
        let gone = [0, 1, 2]
            .map(|i| p1[i] - p0[i])
            .iter()
            .map(|v| v * v)
            .sum::<f32>()
            .sqrt();
        assert!((gone - 5.0).abs() < 1e-3, "{gone}");
    }

    #[test]
    fn a_stalled_frame_is_capped() {
        let start = SceneCamera::framing(CANVAS);
        let stalled = step(start, CANVAS, held(&[Move::Forward]), 100.0, 5.0);
        let gone = get(start.position)[2] - get(stalled.position)[2];
        assert!((gone - 100.0 * MAX_STEP_S).abs() < 1e-3);
    }

    #[test]
    fn level_puts_the_roll_back_and_nothing_else() {
        let start = SceneCamera::framing(CANVAS);
        let rolled = step(start, CANVAS, held(&[Move::RollLeft, Move::Up]), 100.0, 0.1);
        assert_ne!(rolled.roll_deg, start.roll_deg);
        let levelled = level(rolled);
        assert_eq!(levelled.roll_deg.get(), 0.0);
        assert_eq!(levelled.position, rolled.position);
        assert_eq!(levelled.target, rolled.target);
    }

    #[test]
    fn a_small_room_flies_slower_than_a_wide_set() {
        let room = [(item(960.0, 540.0, 0.0, 300.0, 200.0), Shape::Box)];
        let set = [(item(960.0, 540.0, 0.0, 6000.0, 4000.0), Shape::Box)];
        let speed = |items: &[(ItemGeometry, Shape)]| {
            base_speed(extent(items.iter().map(|(g, s)| (g, s))), CANVAS)
        };
        assert!(speed(&room) < speed(&set));
        // A flat 300 × 200 card spans its own diagonal.
        let diagonal = 300.0_f32.hypot(200.0);
        assert!((speed(&room) - diagonal / CROSS_SECONDS).abs() < 1e-2);
    }

    #[test]
    fn a_turned_card_counts_by_where_its_corners_are() {
        let mut floor = item(960.0, 540.0, 0.0, 1600.0, 1600.0);
        floor.tilt_deg = jstream_control::protocol::Finite32::new(90.0).expect("finite");
        let wall = item(960.0, 140.0, -800.0, 1600.0, 800.0);
        let room = [(floor, Shape::Box), (wall, Shape::Box)];
        let size = extent(room.iter().map(|(g, s)| (g, s))).expect("placed");
        // 1600 wide, 800 + the floor's line high, 1600 deep.
        let expected = (1600.0_f32 * 1600.0 + 800.0 * 800.0 + 1600.0 * 1600.0).sqrt();
        assert!(
            (size - expected).abs() / expected < 0.02,
            "{size} vs {expected}"
        );
    }

    #[test]
    fn models_set_the_pace_over_a_backdrop_and_count_their_depth() {
        let model = Shape::Model(SolidBounds::of_size(100.0, 100.0, 50.0));
        let items = [
            (item(960.0, 540.0, 5000.0, 40_000.0, 20_000.0), Shape::Box),
            (item(960.0, 540.0, 0.0, 400.0, 300.0), model),
        ];
        let size = extent(items.iter().map(|(g, s)| (g, s))).expect("placed");
        // 400 × 300, and 200 deep: half its width, as its bounds are.
        let expected = (400.0_f32 * 400.0 + 300.0 * 300.0 + 200.0 * 200.0).sqrt();
        assert!((size - expected).abs() < 1e-2, "{size}");
    }

    #[test]
    fn an_empty_scene_flies_at_its_canvas_pace_and_never_crawls() {
        let canvas_pace = 1920.0_f32.hypot(1080.0) / CROSS_SECONDS;
        assert!((base_speed(None, CANVAS) - canvas_pace).abs() < 1e-3);
        assert_eq!(base_speed(Some(1.0), CANVAS), MIN_SPEED);
    }

    #[test]
    fn the_wheel_scales_speed_within_its_range() {
        assert!(rescale(1.0, -100.0) > 1.0, "wheel away speeds up");
        assert!(rescale(1.0, 100.0) < 1.0);
        assert_eq!(rescale(1.0, -1e6), SPEED_RANGE.1);
        assert_eq!(rescale(1.0, 1e6), SPEED_RANGE.0);
    }

    #[test]
    fn look_turns_at_half_an_orbits_rate() {
        let start = SceneCamera::framing(CANVAS);
        assert_eq!(look(start, 20.0, 0.0), camera::look(start, 10.0, 0.0));
        assert_eq!(look(start, 0.0, 0.0).position, start.position);
    }
}
