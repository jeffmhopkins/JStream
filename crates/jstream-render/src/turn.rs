//! An item's orientation as one rotation
//! ([ADR-0166](../../../docs/adr/0166-moves-curve-through-keyframes-turn-as-one-and-may-orbit.md)
//! decision 1).
//!
//! An item's three angles turn it in ADR-0022's order: `rotation_deg` about z first, then
//! `turn_deg` about y, then `tilt_deg` about x, each CSS's `rotateZ`, `rotateY` and
//! `rotateX`. As a unit quaternion that is `qx(tilt) · qy(turn) · qz(rotation)`.
//! [`slerp_angles`] turns a pair along the shortest arc between its two orientations and
//! gives the three angles back, choosing the triple nearest a hint so they never jump by
//! a turn from one frame to the next.

/// A unit quaternion `(w, x, y, z)`.
type Quat = [f64; 4];

/// Below this angle between two orientations, in radians, the arc is drawn as a straight
/// mix of the two, which is then normalised: `sin` of so small an angle loses precision.
const NEARLY_SAME: f64 = 1e-6;

/// How close `|sin turn|` is to 1 before the decomposition treats the turn as a quarter
/// (gimbal lock), where rotation and tilt share one axis.
const GIMBAL: f64 = 1e-9;

/// The orientation `[rotation, turn, tilt]` in degrees, as a unit quaternion.
fn from_angles([rotation, turn, tilt]: [f32; 3]) -> Quat {
    let half = |deg: f32| (f64::from(deg).to_radians() / 2.0).sin_cos();
    let (sz, cz) = half(rotation);
    let (sy, cy) = half(turn);
    let (sx, cx) = half(tilt);
    mul(
        [cx, sx, 0.0, 0.0],
        mul([cy, 0.0, sy, 0.0], [cz, 0.0, 0.0, sz]),
    )
}

fn mul(a: Quat, b: Quat) -> Quat {
    let [aw, ax, ay, az] = a;
    let [bw, bx, by, bz] = b;
    [
        aw * bw - ax * bx - ay * by - az * bz,
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
    ]
}

fn dot(a: Quat, b: Quat) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn normalize(q: Quat) -> Quat {
    let len = dot(q, q).sqrt();
    if len > 0.0 {
        q.map(|v| v / len)
    } else {
        [1.0, 0.0, 0.0, 0.0]
    }
}

/// The orientation `k` of the way from `a` to `b` along the shortest arc. `k` may be
/// outside `[0, 1]` (a spring's overshoot): the arc carries on.
fn slerp(a: Quat, mut b: Quat, k: f64) -> Quat {
    let mut cos = dot(a, b);
    if cos < 0.0 {
        b = b.map(|v| -v);
        cos = -cos;
    }
    let angle = cos.min(1.0).acos();
    if angle < NEARLY_SAME {
        return normalize([0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * k));
    }
    let sin = angle.sin();
    let wa = ((1.0 - k) * angle).sin() / sin;
    let wb = (k * angle).sin() / sin;
    normalize([0, 1, 2, 3].map(|i| a[i] * wa + b[i] * wb))
}

/// `q` as `[rotation, turn, tilt]` in degrees: of the triples that give it, the one
/// nearest `hint`, each angle within half a turn of the hint's.
fn to_angles(q: Quat, hint: [f32; 3]) -> [f32; 3] {
    let [w, x, y, z] = q;
    // The rotation matrix's entries this needs (row, column), for
    // M = Rx(tilt) · Ry(turn) · Rz(rotation).
    let m00 = 1.0 - 2.0 * (y * y + z * z);
    let m01 = 2.0 * (x * y - w * z);
    let m02 = 2.0 * (x * z + w * y);
    let m10 = 2.0 * (x * y + w * z);
    let m11 = 1.0 - 2.0 * (x * x + z * z);
    let m12 = 2.0 * (y * z - w * x);
    let m22 = 1.0 - 2.0 * (x * x + y * y);
    let hint64 = hint.map(|deg| f64::from(deg).to_radians());
    // At most two triples give one orientation; a quarter turn has one.
    let candidates: [Option<[f64; 3]>; 2] = if m02.abs() >= 1.0 - GIMBAL {
        // A quarter turn: rotation and tilt turn about the same axis, so only their sum
        // (or difference) is fixed. Keep the hint's rotation and give the rest to tilt.
        let rotation = hint64[0];
        let (turn, tilt) = if m02 > 0.0 {
            (std::f64::consts::FRAC_PI_2, m10.atan2(m11) - rotation)
        } else {
            (-std::f64::consts::FRAC_PI_2, rotation - m10.atan2(m11))
        };
        [Some([rotation, turn, tilt]), None]
    } else {
        let turn = m02.asin();
        let tilt = (-m12).atan2(m22);
        let rotation = (-m01).atan2(m00);
        let pi = std::f64::consts::PI;
        [
            Some([rotation, turn, tilt]),
            // The same orientation, the other way round.
            Some([rotation + pi, pi - turn, tilt + pi]),
        ]
    };
    let near = |angles: [f64; 3]| {
        let mut out = [0.0; 3];
        for i in 0..3 {
            out[i] = nearest_turn(angles[i], hint64[i]);
        }
        out
    };
    let distance = |angles: &[f64; 3]| {
        angles
            .iter()
            .zip(hint64)
            .map(|(a, h)| (a - h).powi(2))
            .sum::<f64>()
    };
    let best = candidates
        .into_iter()
        .flatten()
        .map(near)
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
        .unwrap_or(hint64);
    best.map(|rad| rad.to_degrees() as f32)
}

/// `angle` moved by whole turns to within half a turn of `hint`, in radians.
fn nearest_turn(angle: f64, hint: f64) -> f64 {
    let tau = std::f64::consts::TAU;
    angle + ((hint - angle) / tau).round() * tau
}

/// The angles `[rotation, turn, tilt]` `k` of the way from `from` to `to` as one
/// rotation, the shortest arc between the two orientations. `hint` is the per-axis
/// answer, the shortest way on each angle, which picks the triple returned. `k = 0`
/// gives `from` and `k = 1` gives `to`, exactly.
pub(crate) fn slerp_angles(from: [f32; 3], to: [f32; 3], k: f32, hint: [f32; 3]) -> [f32; 3] {
    if k == 0.0 {
        return from;
    }
    if k == 1.0 {
        return to;
    }
    let q = slerp(from_angles(from), from_angles(to), f64::from(k));
    to_angles(q, hint)
}

/// The angles `[rotation, turn, tilt]` `k` of the way along a path through `stops`
/// (ADR-0166 decision 2): the spherical Catmull-Rom curve through each stop's
/// orientation, at equal shares of the path, which is [`slerp_angles`] with two stops.
/// `hint` is the per-axis answer, which picks the triple returned. `k = 0` gives the
/// first stop and `k = 1` the last, exactly. `stops` has at least two entries.
pub(crate) fn path_angles(stops: &[[f32; 3]], k: f32, hint: [f32; 3]) -> [f32; 3] {
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
        return hint;
    };
    if k == 0.0 {
        return *first;
    }
    if k == 1.0 {
        return *last;
    }
    let mut q = [[1.0, 0.0, 0.0, 0.0]; crate::transition::MAX_STOPS];
    let n = stops.len().min(q.len());
    for i in 0..n {
        let mut next = from_angles(stops[i]);
        // Each on the same side as the one before, so every leg takes the short way.
        if i > 0 && dot(q[i - 1], next) < 0.0 {
            next = next.map(|v| -v);
        }
        q[i] = next;
    }
    let turned = crate::transition::catmull(&q[..n], k, |a, b, t| slerp_same(a, b, f64::from(t)));
    to_angles(turned, hint)
}

/// [`slerp`] without choosing the short way: `a` and `b` already lie on one side, and a
/// curve's phantom ends and its extrapolated legs must keep the side they are given.
fn slerp_same(a: Quat, b: Quat, k: f64) -> Quat {
    let cos = dot(a, b).clamp(-1.0, 1.0);
    let angle = cos.acos();
    if angle < NEARLY_SAME {
        return normalize([0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * k));
    }
    let sin = angle.sin();
    let wa = ((1.0 - k) * angle).sin() / sin;
    let wb = (k * angle).sin() / sin;
    normalize([0, 1, 2, 3].map(|i| a[i] * wa + b[i] * wb))
}

/// A unit vector `k` of the way along the great circle from `a` to `b` (ADR-0166
/// decision 3: an orbiting camera's direction from its target). `k` may be outside
/// `[0, 1]`. Opposite directions turn about `up`, or about another axis when `up` lies
/// along them.
pub(crate) fn slerp_direction(a: [f64; 3], b: [f64; 3], k: f64, up: [f64; 3]) -> [f64; 3] {
    let d = |u: [f64; 3], v: [f64; 3]| u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
    let unit = |u: [f64; 3]| {
        let len = d(u, u).sqrt();
        if len > 0.0 {
            u.map(|x| x / len)
        } else {
            [0.0, 0.0, 1.0]
        }
    };
    let cos = d(a, b).clamp(-1.0, 1.0);
    let angle = cos.acos();
    if angle < NEARLY_SAME {
        return unit([0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * k));
    }
    // The direction a quarter turn from `a` toward `b` in their plane, or, when they are
    // opposite and have no plane, toward the picture's up.
    let mut toward = [0, 1, 2].map(|i| b[i] - a[i] * cos);
    if d(toward, toward) < 1e-12 {
        let mut axis = up;
        if d(axis, a).abs() > 0.999 {
            axis = [1.0, 0.0, 0.0];
        }
        let along = d(axis, a);
        toward = [0, 1, 2].map(|i| axis[i] - a[i] * along);
    }
    let toward = unit(toward);
    let (sin_k, cos_k) = (k * angle).sin_cos();
    unit([0, 1, 2].map(|i| a[i] * cos_k + toward[i] * sin_k))
}

#[cfg(test)]
mod tests {
    use super::{from_angles, slerp, slerp_angles, to_angles};

    /// ADR-0022's rotation of `(x, y, z)` by `[rotation, turn, tilt]`, as the placement
    /// math applies it.
    fn rotate([rotation, turn, tilt]: [f32; 3], v: [f64; 3]) -> [f64; 3] {
        let rad = |d: f32| f64::from(d).to_radians();
        let (s, c) = rad(rotation).sin_cos();
        let [x, y, z] = v;
        let (x, y) = (x * c - y * s, x * s + y * c);
        let (s, c) = rad(turn).sin_cos();
        let (x, z) = (x * c + z * s, -x * s + z * c);
        let (s, c) = rad(tilt).sin_cos();
        let (y, z) = (y * c - z * s, y * s + z * c);
        [x, y, z]
    }

    fn rotate_q(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
        let [w, x, y, z] = q;
        let p = super::mul(super::mul(q, [0.0, v[0], v[1], v[2]]), [w, -x, -y, -z]);
        [p[1], p[2], p[3]]
    }

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    const SAMPLES: [[f32; 3]; 6] = [
        [0.0, 0.0, 0.0],
        [30.0, 0.0, 0.0],
        [0.0, 40.0, 0.0],
        [0.0, 0.0, -50.0],
        [20.0, -35.0, 70.0],
        [170.0, 80.0, -120.0],
    ];

    #[test]
    fn the_quaternion_turns_as_the_placement_does() {
        for angles in SAMPLES {
            let q = from_angles(angles);
            for v in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
                assert!(close(rotate(angles, v), rotate_q(q, v)), "{angles:?} {v:?}");
            }
        }
    }

    #[test]
    fn angles_come_back_as_they_went_in() {
        for angles in SAMPLES {
            let back = to_angles(from_angles(angles), angles);
            for (a, b) in angles.iter().zip(back) {
                assert!((a - b).abs() < 1e-3, "{angles:?} → {back:?}");
            }
        }
    }

    #[test]
    fn a_quarter_turn_keeps_the_hints_rotation() {
        let angles = [25.0, 90.0, 10.0];
        let back = to_angles(from_angles(angles), angles);
        for v in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
            assert!(close(rotate(angles, v), rotate(back, v)), "{back:?}");
        }
        assert!((back[0] - 25.0).abs() < 1e-3, "{back:?}");
    }

    #[test]
    fn the_ends_are_exact() {
        let (a, b) = ([10.0, 20.0, 30.0], [-40.0, 50.0, 400.0]);
        assert_eq!(slerp_angles(a, b, 0.0, a), a);
        assert_eq!(slerp_angles(a, b, 1.0, b), b);
    }

    #[test]
    fn one_angle_alone_turns_as_the_shortest_way_on_it() {
        let half = slerp_angles([0.0, 10.0, 0.0], [0.0, 70.0, 0.0], 0.5, [0.0, 40.0, 0.0]);
        assert!(close(half.map(f64::from), [0.0, 40.0, 0.0]), "{half:?}");
    }

    #[test]
    fn two_angles_turn_along_one_arc_at_an_even_speed() {
        // A quarter turn about y and a quarter about x, at once: the direct turn is
        // 120° about (1, 1, 1)-ish, and each quarter of the way covers a quarter of it.
        let (a, b) = ([0.0, 0.0, 0.0], [0.0, 90.0, 90.0]);
        let (qa, qb) = (from_angles(a), from_angles(b));
        let total = super::dot(qa, qb).abs().min(1.0).acos();
        let mut last = qa;
        for step in 1..=4 {
            let k = step as f32 / 4.0;
            let angles = slerp_angles(a, b, k, [0.0, 90.0 * k, 90.0 * k]);
            let q = from_angles(angles);
            let moved = super::dot(last, q).abs().min(1.0).acos();
            assert!((moved - total / 4.0).abs() < 1e-4, "step {step}: {moved}");
            // It lies on the arc slerp gives.
            let on_arc = slerp(qa, qb, f64::from(k));
            assert!(super::dot(on_arc, q).abs() > 1.0 - 1e-6, "step {step}");
            last = q;
        }
    }

    #[test]
    fn angles_stay_near_the_hint_across_a_turn() {
        // From 170° to 190° of rotation and a turn of 30° on the way: the answer stays
        // near 180°, never near -180°.
        let mid = slerp_angles(
            [170.0, 0.0, 0.0],
            [190.0, 30.0, 0.0],
            0.5,
            [180.0, 15.0, 0.0],
        );
        assert!((mid[0] - 180.0).abs() < 5.0, "{mid:?}");
    }

    #[test]
    fn a_spring_carries_on_past_the_end() {
        let (a, b) = ([0.0, 0.0, 0.0], [0.0, 40.0, 40.0]);
        let past = slerp_angles(a, b, 1.2, [0.0, 48.0, 48.0]);
        let (qa, qb, qp) = (from_angles(a), from_angles(b), from_angles(past));
        let ab = super::dot(qa, qb).abs().min(1.0).acos();
        let ap = super::dot(qa, qp).abs().min(1.0).acos();
        assert!((ap - ab * 1.2).abs() < 1e-4, "{ap} vs {ab}");
    }
}
