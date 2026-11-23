//! A backdrop's warp aimed straight ahead (ADR-0187): the rotations that lay its sky out
//! around the scene's straight ahead when a warp starts, and hold the direction of
//! travel near it while at warp.
//!
//! A rotation is a 3x3 matrix, rows by columns, that takes a direction in the frame it
//! turns from into the scene's. An item's turn, tilt and rotation make one as
//! [`jstream_render::Environment::sky_axes`] undoes it: the roll about `z`, then the
//! turn about `y`, then the tilt about `x` (ADR-0022).

/// A rotation, rows by columns.
pub(crate) type Rotation = [[f64; 3]; 3];

/// The rotation that turns nothing.
#[cfg(test)]
pub(crate) const IDENTITY: Rotation = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// The rotation an item's `turn_deg`, `tilt_deg` and `rotation_deg` make, as
/// [`jstream_render::Environment`] takes them.
pub(crate) fn from_angles([turn_deg, tilt_deg, roll_deg]: [f32; 3]) -> Rotation {
    let rad = |deg: f32| {
        if deg.is_finite() {
            f64::from(deg.clamp(-1.0e6, 1.0e6)).to_radians()
        } else {
            0.0
        }
    };
    let (st, ct) = rad(tilt_deg).sin_cos();
    let (su, cu) = rad(turn_deg).sin_cos();
    let (sr, cr) = rad(roll_deg).sin_cos();
    let tilt = [[1.0, 0.0, 0.0], [0.0, ct, -st], [0.0, st, ct]];
    let turn = [[cu, 0.0, su], [0.0, 1.0, 0.0], [-su, 0.0, cu]];
    let roll = [[cr, -sr, 0.0], [sr, cr, 0.0], [0.0, 0.0, 1.0]];
    multiply(&multiply(&tilt, &turn), &roll)
}

/// The turn, tilt and rotation, in degrees, that make `rotation`: the inverse of
/// [`from_angles`], with the turn within -90 to 90 degrees.
pub(crate) fn to_angles(rotation: &Rotation) -> [f32; 3] {
    let m = rotation;
    let turn = m[0][2].clamp(-1.0, 1.0).asin();
    let (tilt, roll) = if m[0][2].abs() < 1.0 - 1e-9 {
        ((-m[1][2]).atan2(m[2][2]), (-m[0][1]).atan2(m[0][0]))
    } else {
        // Turned a quarter: the tilt and the roll spin about one axis, so the roll
        // takes it all.
        (0.0, m[1][0].atan2(m[1][1]))
    };
    [turn, tilt, roll].map(|a| a.to_degrees() as f32)
}

/// `a` after `b`: a direction turned by `b`, then by `a`.
pub(crate) fn multiply(a: &Rotation, b: &Rotation) -> Rotation {
    let mut out = [[0.0; 3]; 3];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    out
}

/// The inverse of `rotation`.
pub(crate) fn inverse(rotation: &Rotation) -> Rotation {
    let m = rotation;
    [
        [m[0][0], m[1][0], m[2][0]],
        [m[0][1], m[1][1], m[2][1]],
        [m[0][2], m[1][2], m[2][2]],
    ]
}

/// `rotation`, turned the least that brings the direction it takes straight ahead
/// (`-z`) to within `limit_deg` of the scene's straight ahead; as it is when that is
/// already within it. A roll about the direction is left as it is.
pub(crate) fn held_ahead(rotation: &Rotation, limit_deg: f32) -> Rotation {
    let limit = if limit_deg.is_finite() {
        f64::from(limit_deg.clamp(0.0, 180.0)).to_radians()
    } else {
        std::f64::consts::PI
    };
    // Where the sky's straight ahead points: minus the third column.
    let ahead = [-rotation[0][2], -rotation[1][2], -rotation[2][2]];
    // Its angle from the scene's straight ahead, [0, 0, -1].
    let angle = (-ahead[2]).clamp(-1.0, 1.0).acos();
    if angle <= limit {
        return *rotation;
    }
    // Turn it back about the axis at right angles to both, `ahead` x `-z`.
    let axis = [-ahead[1], ahead[0], 0.0];
    let length = (axis[0] * axis[0] + axis[1] * axis[1]).sqrt();
    let [x, y, z] = if length > 1e-12 {
        [axis[0] / length, axis[1] / length, 0.0]
    } else {
        // Straight behind: any axis at right angles does; take `y`.
        [0.0, 1.0, 0.0]
    };
    let (s, c) = (angle - limit).sin_cos();
    let t = 1.0 - c;
    let back = [
        [t * x * x + c, t * x * y - s * z, t * x * z + s * y],
        [t * x * y + s * z, t * y * y + c, t * y * z - s * x],
        [t * x * z - s * y, t * y * z + s * x, t * z * z + c],
    ];
    multiply(&back, rotation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &Rotation, b: &Rotation) -> bool {
        a.iter()
            .flatten()
            .zip(b.iter().flatten())
            .all(|(x, y)| (x - y).abs() < 1e-6)
    }

    fn turn(rotation: &Rotation, v: [f64; 3]) -> [f64; 3] {
        let m = rotation;
        [0, 1, 2].map(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
    }

    /// The rotation matches what `sky_axes` undoes: a direction taken back through the
    /// item's rotation by it, then turned by this, is where it was.
    #[test]
    fn the_rotation_is_the_one_the_sky_undoes() {
        let angles = [35.0_f32, -20.0, 70.0];
        let environment = jstream_render::Environment {
            sky: jstream_render::Color::BLACK,
            horizon: jstream_render::Color::BLACK,
            ground: jstream_render::Color::BLACK,
            intensity: 1.0,
            turn_deg: angles[0],
            tilt_deg: angles[1],
            roll_deg: angles[2],
            image: None,
        };
        let axes = [[1.0, 0.0, 0.0], [0.3, 0.8, -0.52], [0.0, 0.6, 0.8]];
        let undone = environment.sky_axes(axes);
        let rotation = from_angles(angles);
        for (axis, back) in axes.iter().zip(undone) {
            let again = turn(&rotation, back.map(f64::from));
            for (a, b) in axis.iter().zip(again) {
                assert!((a - b).abs() < 1e-5, "{axis:?} vs {again:?}");
            }
        }
    }

    #[test]
    fn angles_round_trip() {
        for angles in [
            [0.0_f32, 0.0, 0.0],
            [35.0, -20.0, 70.0],
            [-80.0, 150.0, -170.0],
            [12.0, 89.0, 3.0],
        ] {
            let rotation = from_angles(angles);
            assert!(close(&from_angles(to_angles(&rotation)), &rotation));
        }
        // Turned a quarter, the angles still make the same rotation.
        let quarter = from_angles([90.0, 30.0, 10.0]);
        assert!(close(&from_angles(to_angles(&quarter)), &quarter));
        assert!(close(&multiply(&quarter, &inverse(&quarter)), &IDENTITY));
    }

    /// ADR-0187: a turn within the limit is kept, one past it is brought back to it, and
    /// a roll about the travel is free.
    #[test]
    fn the_travel_is_held_within_its_limit() {
        let small = from_angles([6.0, -5.0, 40.0]);
        assert!(close(&held_ahead(&small, 10.0), &small));
        for angles in [[40.0_f32, 0.0, 0.0], [0.0, -70.0, 25.0], [120.0, 30.0, 0.0]] {
            let held = held_ahead(&from_angles(angles), 10.0);
            let ahead = turn(&held, [0.0, 0.0, -1.0]);
            let angle = (-ahead[2]).acos().to_degrees();
            assert!((angle - 10.0).abs() < 1e-6, "{angles:?}: {angle}");
            assert!(close(&multiply(&held, &inverse(&held)), &IDENTITY));
        }
        // A turn to the right is held to the right.
        let held = held_ahead(&from_angles([-40.0, 0.0, 0.0]), 10.0);
        assert!(close(&held, &from_angles([-10.0, 0.0, 0.0])));
        // Straight behind still comes back to the limit.
        let held = held_ahead(&from_angles([180.0, 0.0, 0.0]), 10.0);
        let ahead = turn(&held, [0.0, 0.0, -1.0]);
        assert!(((-ahead[2]).acos().to_degrees() - 10.0).abs() < 1e-6);
        // A limit of 180 holds nothing.
        let any = from_angles([150.0, 60.0, 0.0]);
        assert!(close(&held_ahead(&any, 180.0), &any));
    }
}
