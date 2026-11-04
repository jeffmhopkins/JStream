//! The built-in shapes of ADR-0107 decision 1, each one unit across, centred on the
//! origin, with one material named `"shape"`.
//!
//! Texture coordinates are the usual ones for each shape: a plane and each face of a
//! cube show the whole texture; a sphere, a cylinder's side and a cone's side wrap it once
//! around, its middle (u = 0.5) facing the viewer, top at v = 0; caps show a disc of it.

use std::f32::consts::{PI, TAU};

use super::{Material, Model, ModelError, ModelVertex, Primitive, compute_tangents};

/// A built-in shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shape {
    /// A square facing the viewer, in the x-y plane.
    Plane,
    /// A cube.
    Cube,
    /// A sphere.
    Sphere,
    /// A cylinder standing upright, with caps.
    Cylinder,
    /// A cone, its point up, with a base.
    Cone,
}

impl Shape {
    /// Every shape.
    pub const ALL: [Shape; 5] = [
        Shape::Plane,
        Shape::Cube,
        Shape::Sphere,
        Shape::Cylinder,
        Shape::Cone,
    ];

    /// Its name as a setting spells it.
    pub fn name(self) -> &'static str {
        match self {
            Shape::Plane => "plane",
            Shape::Cube => "cube",
            Shape::Sphere => "sphere",
            Shape::Cylinder => "cylinder",
            Shape::Cone => "cone",
        }
    }

    /// The shape a setting names.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|shape| shape.name() == name)
    }

    /// The shape as a model with `material` (its name is replaced by `"shape"`).
    ///
    /// # Errors
    /// None in practice: the meshes are built in range. A [`ModelError`] would mean a bug.
    pub fn model(self, material: Material) -> Result<Model, ModelError> {
        let mut mesh = Mesh::default();
        match self {
            Shape::Plane => mesh.quad([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 0.0),
            Shape::Cube => {
                // Each face: outward normal, then its own right and down as seen from
                // outside, so its texture reads the right way round.
                let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
                    ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
                    ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
                    ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
                    ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
                    ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
                    ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
                ];
                for (normal, right, down) in faces {
                    mesh.quad(normal, right, down, 0.5);
                }
            }
            Shape::Sphere => mesh.sphere(64, 32),
            Shape::Cylinder => {
                mesh.side(64, 0.5, 0.5);
                mesh.cap(64, -0.5, -1.0);
                mesh.cap(64, 0.5, 1.0);
            }
            Shape::Cone => {
                mesh.side(64, 0.0, 0.5);
                mesh.cap(64, 0.5, 1.0);
            }
        }
        let mut primitive = Primitive {
            vertices: mesh.vertices,
            indices: mesh.indices,
            material: 0,
        };
        compute_tangents(&mut primitive);
        Model::new(
            vec![primitive],
            vec![Material {
                name: "shape".into(),
                ..material
            }],
            vec![],
        )
    }
}

#[derive(Default)]
struct Mesh {
    vertices: Vec<ModelVertex>,
    indices: Vec<u32>,
}

impl Mesh {
    fn push(&mut self, position: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> u32 {
        self.vertices.push(ModelVertex::new(position, normal, uv));
        (self.vertices.len() - 1) as u32
    }

    /// A triangle wound so that it faces along the average of its vertices' normals.
    fn triangle(&mut self, a: u32, b: u32, c: u32) {
        let at = |i: u32| self.vertices[i as usize];
        let (pa, pb, pc) = (at(a).position, at(b).position, at(c).position);
        let e1 = [0, 1, 2].map(|i| pb[i] - pa[i]);
        let e2 = [0, 1, 2].map(|i| pc[i] - pa[i]);
        let cross = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let normal = [0, 1, 2].map(|i| at(a).normal[i] + at(b).normal[i] + at(c).normal[i]);
        let facing: f32 = (0..3).map(|i| cross[i] * normal[i]).sum();
        if facing >= 0.0 {
            self.indices.extend([a, b, c]);
        } else {
            self.indices.extend([a, c, b]);
        }
    }

    /// A unit square `out` along `normal`, `right` and `down` its texture's u and v.
    fn quad(&mut self, normal: [f32; 3], right: [f32; 3], down: [f32; 3], out: f32) {
        let corner = |u: f32, v: f32| {
            [0, 1, 2].map(|i| normal[i] * out + right[i] * (u - 0.5) + down[i] * (v - 0.5))
        };
        let tl = self.push(corner(0.0, 0.0), normal, [0.0, 0.0]);
        let tr = self.push(corner(1.0, 0.0), normal, [1.0, 0.0]);
        let br = self.push(corner(1.0, 1.0), normal, [1.0, 1.0]);
        let bl = self.push(corner(0.0, 1.0), normal, [0.0, 1.0]);
        self.triangle(tl, tr, br);
        self.triangle(tl, br, bl);
    }

    /// The point at longitude `u` (0 to 1, 0.5 facing the viewer, increasing to the
    /// right) on a ring of `radius` at height `y`, and its direction out from the axis.
    fn ring(u: f32, radius: f32, y: f32) -> ([f32; 3], [f32; 3]) {
        let angle = (u - 0.5) * TAU;
        let (sin, cos) = angle.sin_cos();
        ([sin * radius, y, cos * radius], [sin, 0.0, cos])
    }

    fn sphere(&mut self, segments: u32, rings: u32) {
        let first = self.vertices.len() as u32;
        for r in 0..=rings {
            let v = r as f32 / rings as f32;
            let polar = v * PI;
            let (sin_p, cos_p) = polar.sin_cos();
            for s in 0..=segments {
                let u = s as f32 / segments as f32;
                let (around, _) = Self::ring(u, 1.0, 0.0);
                let normal = [around[0] * sin_p, -cos_p, around[2] * sin_p];
                self.push(normal.map(|c| c * 0.5), normal, [u, v]);
            }
        }
        let row = segments + 1;
        for r in 0..rings {
            for s in 0..segments {
                let a = first + r * row + s;
                let (b, c, d) = (a + 1, a + row, a + row + 1);
                if r != 0 {
                    self.triangle(a, b, d);
                }
                if r != rings - 1 {
                    self.triangle(a, d, c);
                }
            }
        }
    }

    /// The side of a cylinder or cone from `top` radius at y = −0.5 to `bottom` radius at
    /// y = 0.5.
    fn side(&mut self, segments: u32, top: f32, bottom: f32) {
        let first = self.vertices.len() as u32;
        // The outward normal of a slanted side leans up by the slope.
        let slope = bottom - top;
        let length = (1.0 + slope * slope).sqrt();
        for s in 0..=segments {
            let u = s as f32 / segments as f32;
            for (radius, y, v) in [(top, -0.5, 0.0), (bottom, 0.5, 1.0)] {
                let (at, out) = Self::ring(u, radius, y);
                let normal = [out[0] / length, -slope / length, out[2] / length];
                self.push(at, normal, [u, v]);
            }
        }
        for s in 0..segments {
            let a = first + s * 2;
            let (b, c, d) = (a + 1, a + 2, a + 3);
            if top > 0.0 {
                self.triangle(a, c, d);
            }
            self.triangle(a, d, b);
        }
    }

    /// A disc of radius 0.5 at height `y`, facing `side` along y (−1 up, 1 down).
    fn cap(&mut self, segments: u32, y: f32, side: f32) {
        let normal = [0.0, side, 0.0];
        let center = self.push([0.0, y, 0.0], normal, [0.5, 0.5]);
        let first = self.vertices.len() as u32;
        for s in 0..=segments {
            let u = s as f32 / segments as f32;
            let (at, out) = Self::ring(u, 0.5, y);
            self.push(at, normal, [0.5 + out[0] * 0.5, 0.5 + out[2] * 0.5]);
        }
        for s in 0..segments {
            self.triangle(center, first + s, first + s + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    #[test]
    fn every_shape_is_a_unit_across_and_centred() {
        for shape in Shape::ALL {
            let model = shape.model(Material::default()).unwrap();
            let bounds = model.bounds();
            for c in 0..2 {
                assert!((bounds.min[c] + 0.5).abs() < 1e-4, "{shape:?} {bounds:?}");
                assert!((bounds.max[c] - 0.5).abs() < 1e-4, "{shape:?} {bounds:?}");
            }
            assert_eq!(model.materials()[0].name, "shape");
            assert_eq!(Shape::from_name(shape.name()), Some(shape));
        }
    }

    #[test]
    fn every_triangle_of_a_closed_shape_faces_out() {
        for shape in [Shape::Cube, Shape::Sphere, Shape::Cylinder, Shape::Cone] {
            let model = shape.model(Material::default()).unwrap();
            let p = &model.primitives()[0];
            for t in p.indices.chunks(3) {
                let at = |i: u32| p.vertices[i as usize].position;
                let (a, b, c) = (at(t[0]), at(t[1]), at(t[2]));
                let n = cross(
                    [0, 1, 2].map(|i| b[i] - a[i]),
                    [0, 1, 2].map(|i| c[i] - a[i]),
                );
                let centroid = [0, 1, 2].map(|i| (a[i] + b[i] + c[i]) / 3.0);
                let out: f32 = (0..3).map(|i| n[i] * centroid[i]).sum();
                assert!(out > 0.0, "{shape:?} has an inward triangle {t:?}");
            }
        }
    }

    #[test]
    fn the_plane_faces_the_viewer_with_its_texture_upright() {
        let model = Shape::Plane.model(Material::default()).unwrap();
        let p = &model.primitives()[0];
        let top_left = p.vertices.iter().find(|v| v.uv == [0.0, 0.0]).unwrap();
        assert_eq!(top_left.position, [-0.5, -0.5, 0.0]);
        assert_eq!(top_left.normal, [0.0, 0.0, 1.0]);
    }
}
