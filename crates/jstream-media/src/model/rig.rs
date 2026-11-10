//! A model's rig (ADR-0151 decision 7): its node tree, its skins' joints and its
//! animation clips, which pose it at a time.
//!
//! A rigged model's vertices are in bind space: a vertex names up to four palette slots
//! with weights, and the renderer places it at the weighted sum of those slots' matrices
//! times its position. A slot is a node's world matrix, times a skin joint's inverse
//! bind matrix for a skinned mesh. [`Rig::pose`] works the palette out for a clip at a
//! time, and [`Rig::rest`] for the rest pose, which is what a model's bounds are taken in.
//! Both are in the scene's axes (x right, y down, z toward the viewer): the rig is kept
//! in glTF's (y up), and each slot's matrix is turned by the flip on both sides.

use thiserror::Error;

/// A column-major 4x4 matrix, as glTF stores them and the GPU reads them.
pub type Mat4 = [f32; 16];

/// The most joints a skin may have (ADR-0151 decision 7).
pub const MAX_JOINTS: usize = 256;
/// The most slots a palette may have: every mesh node's, and every skin's joints.
pub const MAX_SLOTS: usize = 4096;
/// The most nodes a rig may have.
pub const MAX_NODES: usize = 4096;
/// The most keyframe values a rig's clips may hold in all.
pub const MAX_KEY_VALUES: usize = 16 * 1024 * 1024;

/// Why a rig can't be built.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RigError {
    /// A node's parent comes after it, or doesn't exist.
    #[error("node {node} has parent {parent}, which isn't before it")]
    Parent {
        /// The node.
        node: usize,
        /// Its parent.
        parent: usize,
    },
    /// A slot or channel names a node the rig doesn't have.
    #[error("{what} names node {node} of {nodes}")]
    Node {
        /// What names it.
        what: &'static str,
        /// The node.
        node: usize,
        /// How many nodes the rig has.
        nodes: usize,
    },
    /// A channel's keyframes don't match its times, or its times aren't ascending and
    /// finite.
    #[error("channel {channel} of clip `{clip}` has keyframes that don't match its times")]
    Keys {
        /// The clip.
        clip: String,
        /// The channel.
        channel: usize,
    },
    /// The rig is past one of its caps.
    #[error("the rig has {got} {what}, more than the {limit} a rig may have")]
    TooLarge {
        /// What is counted.
        what: &'static str,
        /// How many it has.
        got: usize,
        /// The cap.
        limit: usize,
    },
}

/// A node's own transform when no clip moves it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rest {
    /// A matrix, which no clip moves (glTF's rule).
    Matrix([f64; 16]),
    /// A translation, a unit quaternion rotation (x, y, z, w) and a scale.
    Trs {
        /// Translation.
        translation: [f64; 3],
        /// Rotation.
        rotation: [f64; 4],
        /// Scale.
        scale: [f64; 3],
    },
}

/// One node of the tree, in glTF's axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RigNode {
    /// Its parent, which comes before it, or `None` for a root.
    pub parent: Option<usize>,
    /// Its own transform at rest.
    pub rest: Rest,
}

/// One palette slot: a node's world matrix, times an inverse bind matrix (the identity
/// for a mesh node's own slot).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    /// The node.
    pub node: usize,
    /// The inverse bind matrix, column-major.
    pub inverse_bind: [f64; 16],
}

/// What a channel moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// The node's translation (3 values a key).
    Translation,
    /// Its rotation (a quaternion, 4 values a key).
    Rotation,
    /// Its scale (3 values a key).
    Scale,
}

impl Path {
    fn width(self) -> usize {
        match self {
            Path::Rotation => 4,
            Path::Translation | Path::Scale => 3,
        }
    }
}

/// How a channel goes from one key to the next (glTF's sampler interpolation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    /// Holds each key until the next.
    Step,
    /// Straight between keys; a rotation is slerped.
    Linear,
    /// A cubic Hermite spline: each key holds an in-tangent, a value and an
    /// out-tangent.
    CubicSpline,
}

/// One animated property of one node.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    /// The node it moves.
    pub node: usize,
    /// What it moves.
    pub path: Path,
    /// How it interpolates.
    pub interpolation: Interpolation,
    /// Key times in seconds, ascending.
    pub times: Vec<f32>,
    /// The keys' values: the path's width each, or three times that for a cubic spline.
    pub values: Vec<f32>,
}

/// A named animation.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    /// Its name, which the `animation` setting picks it by.
    pub name: String,
    /// Its channels.
    pub channels: Vec<Channel>,
    /// Its length in seconds: its last key.
    pub duration: f64,
}

/// A checked rig.
#[derive(Clone, Debug, PartialEq)]
pub struct Rig {
    nodes: Vec<RigNode>,
    slots: Vec<Slot>,
    clips: Vec<Clip>,
}

impl Rig {
    /// Checks and builds a rig. Every clip's duration is set from its last key.
    ///
    /// # Errors
    /// A [`RigError`] naming the first problem: a parent that isn't before its child, a
    /// node out of range, keys that don't match their times, or a cap.
    pub fn new(
        nodes: Vec<RigNode>,
        slots: Vec<Slot>,
        mut clips: Vec<Clip>,
    ) -> Result<Self, RigError> {
        let large = |what, got, limit| RigError::TooLarge { what, got, limit };
        if nodes.len() > MAX_NODES {
            return Err(large("nodes", nodes.len(), MAX_NODES));
        }
        if slots.len() > MAX_SLOTS {
            return Err(large("palette slots", slots.len(), MAX_SLOTS));
        }
        for (node, n) in nodes.iter().enumerate() {
            if let Some(parent) = n.parent
                && parent >= node
            {
                return Err(RigError::Parent { node, parent });
            }
        }
        let in_range = |what, node: usize| {
            if node < nodes.len() {
                Ok(())
            } else {
                Err(RigError::Node {
                    what,
                    node,
                    nodes: nodes.len(),
                })
            }
        };
        for slot in &slots {
            in_range("a palette slot", slot.node)?;
        }
        let mut values = 0usize;
        for clip in &mut clips {
            let mut duration = 0.0f64;
            for (channel, c) in clip.channels.iter().enumerate() {
                in_range("a channel", c.node)?;
                let per_key = c.path.width()
                    * if c.interpolation == Interpolation::CubicSpline {
                        3
                    } else {
                        1
                    };
                let ascending = c.times.windows(2).all(|w| w[0] <= w[1]);
                let finite = c.times.iter().chain(&c.values).all(|v| v.is_finite());
                if c.times.is_empty()
                    || c.values.len() != c.times.len() * per_key
                    || !ascending
                    || !finite
                {
                    return Err(RigError::Keys {
                        clip: clip.name.clone(),
                        channel,
                    });
                }
                values = values.saturating_add(c.values.len() + c.times.len());
                duration = duration.max(f64::from(*c.times.last().unwrap_or(&0.0)));
            }
            clip.duration = duration;
        }
        if values > MAX_KEY_VALUES {
            return Err(large("keyframe values", values, MAX_KEY_VALUES));
        }
        Ok(Self {
            nodes,
            slots,
            clips,
        })
    }

    /// How many palette slots it has: the length of [`Self::rest`] and [`Self::pose`].
    pub fn slots(&self) -> usize {
        self.slots.len()
    }

    /// Its clips.
    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    /// The clip named `name`.
    pub fn clip(&self, name: &str) -> Option<usize> {
        self.clips.iter().position(|c| c.name == name)
    }

    /// The palette at rest.
    pub fn rest(&self) -> Vec<Mat4> {
        let locals: Vec<[f64; 16]> = self.nodes.iter().map(|n| rest_local(&n.rest)).collect();
        self.palette(&locals)
    }

    /// The palette for clip `clip` at `seconds` into it, which the caller has looped or
    /// held: a time before the first key takes the first, one after the last the last.
    /// A clip out of range poses the rest.
    pub fn pose(&self, clip: usize, seconds: f64) -> Vec<Mat4> {
        let Some(clip) = self.clips.get(clip) else {
            return self.rest();
        };
        let mut trs: Vec<Option<Trs>> = self
            .nodes
            .iter()
            .map(|n| match n.rest {
                Rest::Matrix(_) => None,
                Rest::Trs {
                    translation,
                    rotation,
                    scale,
                } => Some(Trs {
                    translation,
                    rotation,
                    scale,
                }),
            })
            .collect();
        for c in &clip.channels {
            let Some(Some(node)) = trs.get_mut(c.node) else {
                continue;
            };
            let value = sample(c, seconds);
            match c.path {
                Path::Translation => node.translation = [value[0], value[1], value[2]],
                Path::Rotation => node.rotation = unit([value[0], value[1], value[2], value[3]]),
                Path::Scale => node.scale = [value[0], value[1], value[2]],
            }
        }
        let locals: Vec<[f64; 16]> = self
            .nodes
            .iter()
            .zip(&trs)
            .map(|(n, t)| match t {
                Some(t) => compose(t.translation, t.rotation, t.scale),
                None => rest_local(&n.rest),
            })
            .collect();
        self.palette(&locals)
    }

    fn palette(&self, locals: &[[f64; 16]]) -> Vec<Mat4> {
        let mut worlds: Vec<[f64; 16]> = Vec::with_capacity(locals.len());
        for (n, local) in self.nodes.iter().zip(locals) {
            let world = match n.parent {
                Some(parent) => multiply(&worlds[parent], local),
                None => *local,
            };
            worlds.push(world);
        }
        self.slots
            .iter()
            .map(|s| {
                let m = multiply(&worlds[s.node], &s.inverse_bind);
                // y up to y down on both sides: negate row 1 and column 1.
                std::array::from_fn(|i| {
                    let (c, r) = (i / 4, i % 4);
                    let sign = if (c == 1) != (r == 1) { -1.0 } else { 1.0 };
                    (m[i] * sign) as f32
                })
            })
            .collect()
    }
}

/// Where a vertex stands under `palette`: the weighted sum of its slots' matrices times
/// its position, or the position itself with no weight.
pub fn skin(palette: &[Mat4], joints: [u16; 4], weights: [f32; 4], p: [f32; 3]) -> [f32; 3] {
    if weights.iter().all(|&w| w == 0.0) {
        return p;
    }
    let mut out = [0.0f32; 3];
    for (j, w) in joints.into_iter().zip(weights) {
        if w == 0.0 {
            continue;
        }
        let Some(m) = palette.get(usize::from(j)) else {
            continue;
        };
        for (r, o) in out.iter_mut().enumerate() {
            *o += w * (m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r]);
        }
    }
    out
}

#[derive(Clone, Copy)]
struct Trs {
    translation: [f64; 3],
    rotation: [f64; 4],
    scale: [f64; 3],
}

fn rest_local(rest: &Rest) -> [f64; 16] {
    match *rest {
        Rest::Matrix(m) => m,
        Rest::Trs {
            translation,
            rotation,
            scale,
        } => compose(translation, rotation, scale),
    }
}

/// T × R × S, column-major.
pub fn compose(t: [f64; 3], q: [f64; 4], s: [f64; 3]) -> [f64; 16] {
    let [x, y, z, w] = q;
    let r = [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y + z * w),
        2.0 * (x * z - y * w),
        2.0 * (x * y - z * w),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z + x * w),
        2.0 * (x * z + y * w),
        2.0 * (y * z - x * w),
        1.0 - 2.0 * (x * x + y * y),
    ];
    [
        r[0] * s[0],
        r[1] * s[0],
        r[2] * s[0],
        0.0,
        r[3] * s[1],
        r[4] * s[1],
        r[5] * s[1],
        0.0,
        r[6] * s[2],
        r[7] * s[2],
        r[8] * s[2],
        0.0,
        t[0],
        t[1],
        t[2],
        1.0,
    ]
}

/// `a × b`, column-major.
pub fn multiply(a: &[f64; 16], b: &[f64; 16]) -> [f64; 16] {
    std::array::from_fn(|i| {
        let (c, r) = (i / 4, i % 4);
        (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum()
    })
}

fn unit(q: [f64; 4]) -> [f64; 4] {
    let length = q.iter().map(|c| c * c).sum::<f64>().sqrt();
    if length < 1e-12 || !length.is_finite() {
        return [0.0, 0.0, 0.0, 1.0];
    }
    q.map(|c| c / length)
}

/// A channel's value at `t` seconds, as `f64`s, the path's width of them.
fn sample(c: &Channel, t: f64) -> Vec<f64> {
    let width = c.path.width();
    let cubic = c.interpolation == Interpolation::CubicSpline;
    // The value of key `k`: the middle of its three parts for a cubic spline.
    let value = |k: usize| -> Vec<f64> {
        let at = if cubic {
            k * 3 * width + width
        } else {
            k * width
        };
        c.values[at..at + width]
            .iter()
            .map(|&v| f64::from(v))
            .collect()
    };
    let last = c.times.len() - 1;
    let first_time = f64::from(c.times[0]);
    if t <= first_time || last == 0 {
        return value(0);
    }
    if t >= f64::from(c.times[last]) {
        return value(last);
    }
    // The key at or before `t`: the last whose time isn't past it.
    let k = c
        .times
        .partition_point(|&k| f64::from(k) <= t)
        .saturating_sub(1)
        .min(last - 1);
    let (t0, t1) = (f64::from(c.times[k]), f64::from(c.times[k + 1]));
    let span = t1 - t0;
    let s = if span > 0.0 { (t - t0) / span } else { 0.0 };
    match c.interpolation {
        Interpolation::Step => value(k),
        Interpolation::Linear => {
            let (a, b) = (value(k), value(k + 1));
            if c.path == Path::Rotation {
                slerp(&a, &b, s)
            } else {
                a.iter().zip(&b).map(|(a, b)| a + (b - a) * s).collect()
            }
        }
        Interpolation::CubicSpline => {
            let part = |k: usize, p: usize| -> Vec<f64> {
                let at = k * 3 * width + p * width;
                c.values[at..at + width]
                    .iter()
                    .map(|&v| f64::from(v))
                    .collect()
            };
            let (v0, out0) = (part(k, 1), part(k, 2));
            let (in1, v1) = (part(k + 1, 0), part(k + 1, 1));
            let (s2, s3) = (s * s, s * s * s);
            let h00 = 2.0 * s3 - 3.0 * s2 + 1.0;
            let h10 = s3 - 2.0 * s2 + s;
            let h01 = -2.0 * s3 + 3.0 * s2;
            let h11 = s3 - s2;
            let out: Vec<f64> = (0..width)
                .map(|i| h00 * v0[i] + h10 * span * out0[i] + h01 * v1[i] + h11 * span * in1[i])
                .collect();
            if c.path == Path::Rotation {
                unit([out[0], out[1], out[2], out[3]]).to_vec()
            } else {
                out
            }
        }
    }
}

/// Spherical interpolation between two unit quaternions, the short way round.
fn slerp(a: &[f64], b: &[f64], s: f64) -> Vec<f64> {
    let a = unit([a[0], a[1], a[2], a[3]]);
    let mut b = unit([b[0], b[1], b[2], b[3]]);
    let mut dot: f64 = (0..4).map(|i| a[i] * b[i]).sum();
    if dot < 0.0 {
        b = b.map(|c| -c);
        dot = -dot;
    }
    if dot > 0.9995 {
        let mixed: [f64; 4] = std::array::from_fn(|i| a[i] + (b[i] - a[i]) * s);
        return unit(mixed).to_vec();
    }
    let theta = dot.clamp(-1.0, 1.0).acos();
    let sin = theta.sin();
    let (wa, wb) = (((1.0 - s) * theta).sin() / sin, (s * theta).sin() / sin);
    (0..4).map(|i| a[i] * wa + b[i] * wb).collect()
}

#[cfg(test)]
mod tests;
