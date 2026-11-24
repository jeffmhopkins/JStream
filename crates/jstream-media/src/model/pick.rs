//! Where a ray first meets a model's surface, on the CPU: what a camera's autofocus
//! measures a model by (ADR-0176 decision 5, as amended by ADR-0218).
//!
//! The surface is the one the model draws: a triangle of a blended material is passed
//! over (it is seen through), a masked one counts as opaque, and a single-sided
//! triangle counts only from its front. A model without a pose is searched through a
//! bounding volume hierarchy built once, on a thread of its own after it is first asked,
//! and shared by every clone of the model; a posed (rigged) model is skinned by its palette and searched
//! triangle by triangle, since its pose changes every frame.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use super::rig::{self, Mat4};
use super::{AlphaMode, Model};

/// Triangles in a leaf of the hierarchy.
const LEAF: usize = 4;

/// A model's triangles in its rest pose, ordered so each node of the hierarchy holds a
/// run of them.
#[derive(Debug)]
pub(super) struct Bvh {
    nodes: Vec<Node>,
    triangles: Vec<Triangle>,
    /// The rig's rest palette the triangles are posed by (empty without a rig): a model
    /// posed by exactly it is searched through the hierarchy too.
    rest: Vec<Mat4>,
}

/// The deepest a traversal goes: a median split halves every run, so a model of
/// [`super::MAX_TRIANGLES`] is about 20 levels deep.
const MAX_DEPTH: usize = 64;

/// A node: its box, and either its first child (the second follows it) or its run of
/// triangles.
#[derive(Clone, Copy, Debug)]
struct Node {
    min: [f32; 3],
    max: [f32; 3],
    /// For a leaf, its first triangle; for an inner node, its first child.
    first: u32,
    /// For a leaf, how many triangles it has; 0 for an inner node.
    count: u32,
}

/// One triangle, its corners in the model's space and the material it is drawn with.
#[derive(Clone, Copy, Debug)]
struct Triangle {
    corners: [[f32; 3]; 3],
    material: u32,
}

/// The hierarchy a model builds after its first [`Model::ray_hit`], kept behind an `Arc`
/// so the model's clones share it, with whether its build has started. It is not part
/// of what a model is, so any two compare equal.
#[derive(Clone, Debug, Default)]
pub(super) struct Picker(Arc<Built>);

/// A hierarchy, once built, and whether its build has started.
#[derive(Debug, Default)]
struct Built {
    bvh: OnceLock<Bvh>,
    started: AtomicBool,
}

impl PartialEq for Picker {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Model {
    /// How far along the ray from `origin` in direction `dir` (both in the model's own
    /// space) the ray first meets the surface the model draws, in units of `dir`: the
    /// nearest `t > min_t` with `origin + t·dir` on a triangle that is drawn opaque and
    /// faces the ray (or is double-sided). `None` when the ray meets none.
    ///
    /// A face's front is its outward side in the model's own space, the side its winding
    /// points out of, and a placement that mirrors the model doesn't change which side
    /// that is: the renderer draws a mirrored model's outward faces too.
    ///
    /// `palette` poses a rigged model (its rig's slots, as [`rig::skin`] reads them);
    /// `None`, or the rig's rest palette, keeps the rest pose, which is searched through
    /// a hierarchy built on a thread of its own the first time it is asked (tens of
    /// milliseconds for a few hundred thousand triangles), so the caller, a render
    /// thread, never waits on it: until it is built, this is `None`, as for a ray that
    /// meets nothing. [`Self::prepare_picking`] builds it at once. Any other pose (a clip
    /// playing) is skinned and searched triangle by triangle, which costs time in
    /// proportion to the model's triangles.
    pub fn ray_hit(
        self: &Arc<Self>,
        origin: [f64; 3],
        dir: [f64; 3],
        palette: Option<&[Mat4]>,
        min_t: f64,
    ) -> Option<f64> {
        let ray = Ray::new(origin, dir, min_t);
        let palette = palette.filter(|_| self.rig.is_some());
        let built = &self.picker.0;
        if let Some(bvh) = built.bvh.get() {
            return match palette {
                Some(palette) if palette != bvh.rest.as_slice() => self.posed_hit(&ray, palette),
                _ => bvh.hit(self, &ray),
            };
        }
        if !built.started.swap(true, Ordering::AcqRel) {
            let model = Arc::clone(self);
            let spawned = std::thread::Builder::new()
                .name("model-pick".into())
                .spawn(move || model.prepare_picking());
            if spawned.is_err() {
                // No thread to build it on: a later ask tries again.
                built.started.store(false, Ordering::Release);
            }
        }
        None
    }

    /// Builds the hierarchy [`Self::ray_hit`] searches an unposed model through, here and
    /// now, unless it is built already; the model's clones share it.
    pub fn prepare_picking(&self) {
        self.picker.0.bvh.get_or_init(|| Bvh::build(self));
    }

    /// Whether the hierarchy is built, so [`Self::ray_hit`] answers for an unposed model.
    pub fn picking_ready(&self) -> bool {
        self.picker.0.bvh.get().is_some()
    }

    /// [`Self::ray_hit`] for a posed model: every triangle, skinned.
    fn posed_hit(&self, ray: &Ray, palette: &[Mat4]) -> Option<f64> {
        let mut nearest: Option<f64> = None;
        for p in &self.primitives {
            if !ray.counts(&self.materials[p.material].alpha_mode) {
                continue;
            }
            let double = self.materials[p.material].double_sided;
            for t in p.indices.chunks_exact(3) {
                let corners = [t[0], t[1], t[2]].map(|i| {
                    let v = &p.vertices[i as usize];
                    rig::skin(palette, v.joints, v.weights, v.position)
                });
                if let Some(at) = ray.meets(corners, double)
                    && nearest.is_none_or(|n| at < n)
                {
                    nearest = Some(at);
                }
            }
        }
        nearest
    }
}

/// A ray in a model's space, with what a hit needs precomputed.
struct Ray {
    origin: [f64; 3],
    dir: [f64; 3],
    inverse: [f64; 3],
    /// The nearest a hit may be.
    min_t: f64,
}

impl Ray {
    fn new(origin: [f64; 3], dir: [f64; 3], min_t: f64) -> Self {
        Self {
            origin,
            dir,
            inverse: dir.map(|d| 1.0 / d),
            min_t,
        }
    }

    /// Whether a material with `alpha` is one the ray can stop on: a blended surface is
    /// seen through.
    fn counts(&self, alpha: &AlphaMode) -> bool {
        !matches!(alpha, AlphaMode::Blend)
    }

    /// Where the ray meets the triangle `corners` (Möller–Trumbore), when it meets its
    /// front, or either side of a `double_sided` one.
    fn meets(&self, corners: [[f32; 3]; 3], double_sided: bool) -> Option<f64> {
        let [a, b, c] = corners.map(|p| p.map(f64::from));
        let e1 = sub(b, a);
        let e2 = sub(c, a);
        // The front is the side `e1 × e2` points out of; the ray sees it when it runs
        // against that normal. Both are in the model's own space, so a mirroring
        // placement changes neither.
        let front = dot(cross(e1, e2), self.dir) < 0.0;
        if !front && !double_sided {
            return None;
        }
        let p = cross(self.dir, e2);
        let det = dot(e1, p);
        if det.abs() < 1e-12 {
            return None;
        }
        let inv = 1.0 / det;
        let s = sub(self.origin, a);
        let u = dot(s, p) * inv;
        if !(0.0..=1.0).contains(&u) {
            return None;
        }
        let q = cross(s, e1);
        let v = dot(self.dir, q) * inv;
        if v < 0.0 || u + v > 1.0 {
            return None;
        }
        let t = dot(e2, q) * inv;
        (t.is_finite() && t > self.min_t).then_some(t)
    }

    /// Where the ray enters the box `min`..`max` (slab test), if it does before
    /// `limit`.
    fn enters(&self, min: [f32; 3], max: [f32; 3], limit: f64) -> Option<f64> {
        let (mut near, mut far) = (0.0f64, limit);
        for i in 0..3 {
            let (lo, hi) = (f64::from(min[i]), f64::from(max[i]));
            if self.dir[i] == 0.0 {
                if self.origin[i] < lo || self.origin[i] > hi {
                    return None;
                }
                continue;
            }
            let t0 = (lo - self.origin[i]) * self.inverse[i];
            let t1 = (hi - self.origin[i]) * self.inverse[i];
            let (t0, t1) = if t0 <= t1 { (t0, t1) } else { (t1, t0) };
            near = near.max(t0);
            far = far.min(t1);
            if near > far {
                return None;
            }
        }
        Some(near)
    }
}

impl Bvh {
    /// The hierarchy over `model`'s triangles in its rest pose.
    fn build(model: &Model) -> Self {
        let rest = model.rig.as_ref().map(|r| r.rest()).unwrap_or_default();
        let mut triangles = Vec::with_capacity(model.triangles());
        for p in &model.primitives {
            for t in p.indices.chunks_exact(3) {
                let corners = [t[0], t[1], t[2]].map(|i| {
                    let v = &p.vertices[i as usize];
                    rig::skin(&rest, v.joints, v.weights, v.position)
                });
                triangles.push(Triangle {
                    corners,
                    material: p.material as u32,
                });
            }
        }
        let mut bvh = Self {
            nodes: Vec::with_capacity(2 * triangles.len() / LEAF + 1),
            triangles,
            rest,
        };
        let count = bvh.triangles.len();
        bvh.nodes.push(Node {
            min: [0.0; 3],
            max: [0.0; 3],
            first: 0,
            count: 0,
        });
        bvh.split(0, 0, count);
        bvh
    }

    /// Fills node `node` with triangles `first..first + count`, splitting it at the
    /// median of their centres along the longest side of their centres' box.
    fn split(&mut self, node: usize, first: usize, count: usize) {
        let run = &mut self.triangles[first..first + count];
        let (mut min, mut max) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        let (mut cmin, mut cmax) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for t in run.iter() {
            for corner in t.corners {
                for i in 0..3 {
                    min[i] = min[i].min(corner[i]);
                    max[i] = max[i].max(corner[i]);
                }
            }
            let c = centre(t);
            for i in 0..3 {
                cmin[i] = cmin[i].min(c[i]);
                cmax[i] = cmax[i].max(c[i]);
            }
        }
        self.nodes[node].min = min;
        self.nodes[node].max = max;
        let extent = [0, 1, 2].map(|i| cmax[i] - cmin[i]);
        if count <= LEAF || extent.iter().all(|&e| e <= 0.0) {
            self.nodes[node].first = first as u32;
            self.nodes[node].count = count as u32;
            return;
        }
        let axis = (0..3)
            .max_by(|&a, &b| extent[a].total_cmp(&extent[b]))
            .unwrap_or(0);
        let half = count / 2;
        run.select_nth_unstable_by(half, |a, b| centre(a)[axis].total_cmp(&centre(b)[axis]));
        let left = self.nodes.len();
        let blank = Node {
            min: [0.0; 3],
            max: [0.0; 3],
            first: 0,
            count: 0,
        };
        self.nodes.push(blank);
        self.nodes.push(blank);
        self.nodes[node].first = left as u32;
        self.nodes[node].count = 0;
        self.split(left, first, half);
        self.split(left + 1, first + half, count - half);
    }

    /// [`Model::ray_hit`] through the hierarchy, `model` giving the materials now.
    fn hit(&self, model: &Model, ray: &Ray) -> Option<f64> {
        let mut nearest = f64::INFINITY;
        // A fixed stack: the render thread allocates nothing per ray.
        let mut stack = [0usize; MAX_DEPTH];
        let mut depth = 1;
        while depth > 0 {
            depth -= 1;
            let index = stack[depth];
            let node = self.nodes[index];
            if ray.enters(node.min, node.max, nearest).is_none() {
                continue;
            }
            if node.count == 0 {
                let first = node.first as usize;
                // Visit the nearer child first, so the farther is often culled.
                let near = |i: usize| {
                    let n = self.nodes[i];
                    ray.enters(n.min, n.max, f64::INFINITY)
                        .unwrap_or(f64::INFINITY)
                };
                let order = if near(first) <= near(first + 1) {
                    [first + 1, first]
                } else {
                    [first, first + 1]
                };
                // Never deeper than the build's halving allows; a deeper node would be
                // skipped rather than overflow.
                if depth + 2 <= MAX_DEPTH {
                    stack[depth] = order[0];
                    stack[depth + 1] = order[1];
                    depth += 2;
                }
                continue;
            }
            let run = node.first as usize..(node.first + node.count) as usize;
            for t in &self.triangles[run] {
                let Some(material) = model.materials.get(t.material as usize) else {
                    continue;
                };
                if !ray.counts(&material.alpha_mode) {
                    continue;
                }
                if let Some(at) = ray.meets(t.corners, material.double_sided)
                    && at < nearest
                {
                    nearest = at;
                }
            }
        }
        nearest.is_finite().then_some(nearest)
    }
}

fn centre(t: &Triangle) -> [f32; 3] {
    [0, 1, 2].map(|i| (t.corners[0][i] + t.corners[1][i] + t.corners[2][i]) / 3.0)
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Material, ModelVertex, Primitive};

    /// A unit square at `z`, facing +z (the viewer), in material `material`.
    fn square(z: f32, material: usize) -> Primitive {
        let v = |x: f32, y: f32| ModelVertex::new([x, y, z], [0.0, 0.0, 1.0], [0.0, 0.0]);
        // Clockwise on the y-down canvas seen from +z: (b − a) × (c − a) points to +z.
        Primitive {
            vertices: vec![v(-1.0, -1.0), v(1.0, -1.0), v(1.0, 1.0), v(-1.0, 1.0)],
            indices: vec![0, 1, 2, 0, 2, 3],
            material,
        }
    }

    fn material(alpha_mode: AlphaMode, double_sided: bool) -> Material {
        Material {
            alpha_mode,
            double_sided,
            ..Material::default()
        }
    }

    fn model(primitives: Vec<Primitive>, materials: Vec<Material>) -> Arc<Model> {
        let model = Model::new(primitives, materials, Vec::new()).expect("model");
        model.prepare_picking();
        Arc::new(model)
    }

    const DOWN_Z: [f64; 3] = [0.0, 0.0, -1.0];

    #[test]
    fn a_ray_meets_the_nearest_front_face() {
        let m = model(
            vec![square(0.0, 0), square(-2.0, 0)],
            vec![material(AlphaMode::Opaque, false)],
        );
        assert_eq!(m.ray_hit([0.2, 0.3, 5.0], DOWN_Z, None, 0.0), Some(5.0));
        // Past the first square, the second is met.
        assert_eq!(m.ray_hit([0.2, 0.3, -1.0], DOWN_Z, None, 0.0), Some(1.0));
        // Beside both, none.
        assert_eq!(m.ray_hit([3.0, 0.0, 5.0], DOWN_Z, None, 0.0), None);
    }

    #[test]
    fn a_back_face_counts_only_when_double_sided() {
        let single = model(
            vec![square(0.0, 0)],
            vec![material(AlphaMode::Opaque, false)],
        );
        let up = [0.0, 0.0, 1.0];
        assert_eq!(single.ray_hit([0.0, 0.0, -5.0], up, None, 0.0), None);
        assert_eq!(
            single.ray_hit([0.0, 0.0, 5.0], DOWN_Z, None, 0.0),
            Some(5.0)
        );
        let double = model(
            vec![square(0.0, 0)],
            vec![material(AlphaMode::Opaque, true)],
        );
        assert_eq!(double.ray_hit([0.0, 0.0, -5.0], up, None, 0.0), Some(5.0));
    }

    /// A rigged model posed by its rest palette is searched through the hierarchy; any
    /// other pose is skinned: a square moved 3 back by its one joint is met there.
    #[test]
    fn a_rigged_model_at_rest_uses_the_hierarchy_and_a_pose_is_skinned() {
        use crate::model::rig::{Rest, Rig, RigNode, Slot};
        const IDENTITY: [f64; 16] = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let rig = Rig::new(
            vec![RigNode {
                parent: None,
                rest: Rest::Matrix(IDENTITY),
            }],
            vec![Slot {
                node: 0,
                inverse_bind: IDENTITY,
            }],
            vec![],
        )
        .expect("rig");
        let rest = rig.rest();
        let mut p = square(0.0, 0);
        for v in &mut p.vertices {
            v.weights = [1.0, 0.0, 0.0, 0.0];
        }
        let m = Model::rigged(
            vec![p],
            vec![material(AlphaMode::Opaque, false)],
            Vec::new(),
            Some(Arc::new(rig)),
        )
        .expect("model");
        m.prepare_picking();
        let m = Arc::new(m);
        assert_eq!(
            m.ray_hit([0.0, 0.0, 5.0], DOWN_Z, Some(&rest), 0.0),
            Some(5.0)
        );
        // The joint moved 3 toward -z: column-major, the translation is in 12..15.
        let mut moved = rest[0];
        moved[14] -= 3.0;
        assert_eq!(
            m.ray_hit([0.0, 0.0, 5.0], DOWN_Z, Some(&[moved]), 0.0),
            Some(8.0)
        );
    }

    /// A surface nearer than `min_t` is passed over, and the next one met.
    #[test]
    fn a_hit_nearer_than_the_minimum_passes_to_the_next() {
        let m = model(
            vec![square(0.0, 0), square(-2.0, 0)],
            vec![material(AlphaMode::Opaque, false)],
        );
        assert_eq!(m.ray_hit([0.0, 0.0, 0.5], DOWN_Z, None, 1.0), Some(2.5));
    }

    #[test]
    fn a_blended_surface_is_seen_through_and_a_masked_one_is_not() {
        let m = model(
            vec![square(0.0, 0), square(-2.0, 1)],
            vec![
                material(AlphaMode::Blend, false),
                material(AlphaMode::Opaque, false),
            ],
        );
        assert_eq!(m.ray_hit([0.0, 0.0, 5.0], DOWN_Z, None, 0.0), Some(7.0));
        // A clone with its materials changed shares the hierarchy, and reads the
        // materials it has now.
        let masked = Arc::new(m.with_materials(|mat| {
            if mat.alpha_mode == AlphaMode::Blend {
                mat.alpha_mode = AlphaMode::Mask { cutoff: 0.5 };
            }
        }));
        assert!(masked.picking_ready());
        assert_eq!(
            masked.ray_hit([0.0, 0.0, 5.0], DOWN_Z, None, 0.0),
            Some(5.0)
        );
    }

    /// An unprepared model answers nothing until its hierarchy is built, on a thread of
    /// its own, then answers.
    #[test]
    fn the_first_ask_builds_on_a_thread_of_its_own() {
        let m = Arc::new(
            Model::new(
                vec![square(0.0, 0)],
                vec![material(AlphaMode::Opaque, false)],
                Vec::new(),
            )
            .expect("model"),
        );
        let _ = m.ray_hit([0.0, 0.0, 5.0], DOWN_Z, None, 0.0);
        let start = std::time::Instant::now();
        while !m.picking_ready() {
            assert!(start.elapsed().as_secs() < 10, "the hierarchy was built");
            std::thread::yield_now();
        }
        assert_eq!(m.ray_hit([0.0, 0.0, 5.0], DOWN_Z, None, 0.0), Some(5.0));
    }

    /// The hierarchy finds what a search of every triangle finds, over a field of many
    /// small squares at different depths.
    #[test]
    fn the_hierarchy_agrees_with_every_triangle() {
        let mut primitives = Vec::new();
        for i in 0..20 {
            for j in 0..20 {
                let z = ((i * 7 + j * 13) % 11) as f32 - 5.0;
                let mut p = square(z, 0);
                for v in &mut p.vertices {
                    v.position[0] = v.position[0] * 0.4 + i as f32;
                    v.position[1] = v.position[1] * 0.4 + j as f32;
                }
                primitives.push(p);
            }
        }
        let m = model(primitives, vec![material(AlphaMode::Opaque, false)]);
        let all = |origin: [f64; 3], dir: [f64; 3]| {
            let ray = Ray::new(origin, dir, 0.0);
            let mut nearest: Option<f64> = None;
            for p in m.primitives() {
                for t in p.indices.chunks_exact(3) {
                    let corners = [t[0], t[1], t[2]].map(|i| p.vertices[i as usize].position);
                    if let Some(at) = ray.meets(corners, false) {
                        nearest = Some(nearest.map_or(at, |n: f64| n.min(at)));
                    }
                }
            }
            nearest
        };
        for k in 0..200 {
            let x = (k % 23) as f64 * 0.9 - 1.0;
            let y = (k % 17) as f64 * 1.2 - 1.0;
            let dir = [
                0.05 * ((k % 5) as f64 - 2.0),
                0.03 * ((k % 7) as f64 - 3.0),
                -1.0,
            ];
            let origin = [x, y, 20.0];
            assert_eq!(
                m.ray_hit(origin, dir, None, 0.0),
                all(origin, dir),
                "ray {k}"
            );
        }
    }
}
