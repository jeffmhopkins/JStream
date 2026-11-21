//! A 3D group's pose: how a group that shares its scene's space places its children and a
//! camera that rides with it (ADR-0164 decisions 2 and 5,
//! docs/adr/0164-3d-groups-share-their-scenes-space.md).
//!
//! The group item maps the group's own canvas into the scene by a uniform scale, a
//! rotation in ADR-0022's order and a move, so a child placed through it is again one
//! [`ItemGeometry`] and a camera placed through it one [`SceneCamera`]: everything that
//! reads a 3D item or a scene's camera takes them unchanged.

use super::camera::{SceneCamera, Vec3, dot};
use super::{Canvas, FitMode, GeometryError, ItemGeometry, Size, rotate_x, rotate_y, rotate_z};
use crate::command::Vec2;
use crate::finite::Finite32;

/// Where a 3D group puts its own space in its scene's (ADR-0164 decision 2): a point of
/// the group's canvas, `p`, lands at `origin + scale · R · (p − centre)`, with the group
/// canvas's centre at `z = 0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupPose {
    /// `R`, row-major: ADR-0022's rotations, `rotation_deg` first, then `turn_deg`,
    /// then `tilt_deg`.
    rotation: [[f64; 3]; 3],
    /// The uniform scale its fit mode gives the group's canvas in the item's box.
    scale: f64,
    /// Where the group canvas's centre lands: the item's centre and `depth_px`.
    origin: [f64; 3],
    /// The group canvas's centre, in its own pixels.
    centre: [f64; 2],
}

/// `R` for ADR-0022's three angles, built from the same rotations the drawn polygon
/// uses, so the two can't disagree.
fn rotation_of(rotation_deg: f32, turn_deg: f32, tilt_deg: f32) -> [[f64; 3]; 3] {
    let column = |x: f32, y: f32, z: f32| {
        let (x, y, z) = rotate_z(x, y, z, rotation_deg);
        let (x, y, z) = rotate_y(x, y, z, turn_deg);
        let (x, y, z) = rotate_x(x, y, z, tilt_deg);
        [f64::from(x), f64::from(y), f64::from(z)]
    };
    let columns = [
        column(1.0, 0.0, 0.0),
        column(0.0, 1.0, 0.0),
        column(0.0, 0.0, 1.0),
    ];
    [0, 1, 2].map(|row| [0, 1, 2].map(|col| columns[col][row]))
}

fn apply(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|row| dot(m[row], v))
}

fn multiply(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    [0, 1, 2].map(|row| [0, 1, 2].map(|col| (0..3).map(|k| a[row][k] * b[k][col]).sum()))
}

/// The three angles, in degrees, of ADR-0022's order that make `m`: `R = Rx(tilt) ·
/// Ry(turn) · Rz(rotation)`. The tilt comes from column 2, and the rotation and turn
/// from `Rx(tilt)ᵀ · R = Ry(turn) · Rz(rotation)`, so each angle is read against the
/// others already taken out. That stays exact near a quarter turn, where the tilt and
/// the rotation fold into one angle: whatever tilt the noise there gives, the other two
/// make up the same matrix.
fn angles_of(m: &[[f64; 3]; 3]) -> (f64, f64, f64) {
    let tilt = (-m[1][2]).atan2(m[2][2]);
    let (st, ct) = tilt.sin_cos();
    let rotation = (ct * m[1][0] + st * m[2][0]).atan2(ct * m[1][1] + st * m[2][1]);
    let turn = m[0][2].atan2(ct * m[2][2] - st * m[1][2]);
    (rotation.to_degrees(), turn.to_degrees(), tilt.to_degrees())
}

fn finite(value: f64) -> Result<Finite32, GeometryError> {
    // `as` rounds to the nearest f32, and saturates to infinity, which `new` rejects.
    Finite32::new(value as f32).ok_or(GeometryError::NotFinite)
}

impl GroupPose {
    /// The pose of a group drawn through `item`, whose own canvas is `canvas`: the scale
    /// its fit mode gives that canvas in the box (`fit` and `stretch` the smaller of the
    /// box's two scales, `fill` the larger), its orientation, and its centre and depth.
    /// Its crop, flips, `billboard`, `surface` and `perspective_px` don't apply
    /// (ADR-0164 decision 3).
    ///
    /// # Errors
    /// [`GeometryError::NotFinite`] when the scale isn't finite and positive.
    pub fn of(item: ItemGeometry, canvas: Canvas) -> Result<Self, GeometryError> {
        let (width, height) = (f64::from(canvas.width), f64::from(canvas.height));
        let x = f64::from(item.size.width.get()) / width;
        let y = f64::from(item.size.height.get()) / height;
        let scale = match item.fit_mode {
            FitMode::Fill => x.max(y),
            _ => x.min(y),
        };
        if !scale.is_finite() || scale <= 0.0 {
            return Err(GeometryError::NotFinite);
        }
        Ok(Self {
            rotation: rotation_of(
                item.rotation_deg.get(),
                item.turn_deg.get(),
                item.tilt_deg.get(),
            ),
            scale,
            origin: [
                f64::from(item.center.x.get()),
                f64::from(item.center.y.get()),
                f64::from(item.depth_px.get()),
            ],
            centre: [width * 0.5, height * 0.5],
        })
    }

    /// The uniform scale the group's space has in its scene.
    pub fn scale(&self) -> f32 {
        self.scale as f32
    }

    /// Where a point of the group's space lands in its scene's.
    pub fn place_point(&self, point: [f64; 3]) -> [f64; 3] {
        let local = [
            (point[0] - self.centre[0]) * self.scale,
            (point[1] - self.centre[1]) * self.scale,
            point[2] * self.scale,
        ];
        let turned = apply(&self.rotation, local);
        [0, 1, 2].map(|i| self.origin[i] + turned[i])
    }

    /// A direction of the group's space turned into its scene's, unscaled.
    pub fn turn(&self, direction: [f64; 3]) -> [f64; 3] {
        apply(&self.rotation, direction)
    }

    /// A child's geometry, in the group's own pixels, placed in the scene's space
    /// (ADR-0164 decision 2): its centre and `depth_px` placed as a point, its
    /// orientation the group's followed by its own, its size scaled. Everything else it
    /// has (fit mode, flips, billboard, surface, `perspective_px`) is kept. A 3D group
    /// among the children is placed the same way, and its own pose taken from the
    /// result, so nested groups compose.
    ///
    /// # Errors
    /// [`GeometryError::NotFinite`] when a placed value overflows `f32`.
    pub fn place(&self, child: ItemGeometry) -> Result<ItemGeometry, GeometryError> {
        let [x, y, z] = self.place_point([
            f64::from(child.center.x.get()),
            f64::from(child.center.y.get()),
            f64::from(child.depth_px.get()),
        ]);
        let own = rotation_of(
            child.rotation_deg.get(),
            child.turn_deg.get(),
            child.tilt_deg.get(),
        );
        let (rotation, turn, tilt) = angles_of(&multiply(&self.rotation, &own));
        Ok(ItemGeometry {
            center: Vec2 {
                x: finite(x)?,
                y: finite(y)?,
            },
            size: Size {
                width: finite(f64::from(child.size.width.get()) * self.scale)?,
                height: finite(f64::from(child.size.height.get()) * self.scale)?,
            },
            rotation_deg: finite(rotation)?,
            turn_deg: finite(turn)?,
            tilt_deg: finite(tilt)?,
            depth_px: finite(z)?,
            ..child
        })
    }

    /// A camera whose numbers are in the group's space, placed in the scene's
    /// (ADR-0164 decision 5): its position and target placed as points, the picture's up
    /// the group's, its roll worked out so the placed camera's picture is turned as the
    /// group turns it, and `focus_px` scaled. Its field of view, depth sort and blur are
    /// kept.
    ///
    /// # Errors
    /// What [`SceneCamera::view`] reports for `camera`, or [`GeometryError::NotFinite`]
    /// when a placed value overflows `f32`.
    pub fn place_camera(
        &self,
        camera: SceneCamera,
        canvas: Canvas,
    ) -> Result<SceneCamera, GeometryError> {
        let own = camera.view(canvas)?;
        let vec3 = |point: [f64; 3]| -> Result<Vec3, GeometryError> {
            Ok(Vec3 {
                x: finite(point[0])?,
                y: finite(point[1])?,
                z: finite(point[2])?,
            })
        };
        let placed = SceneCamera {
            position: vec3(self.place_point(camera.position.get()))?,
            target: vec3(self.place_point(camera.target.get()))?,
            roll_deg: Finite32::ZERO,
            focus_px: camera
                .focus_px
                .map(|focus| finite(f64::from(focus.get()) * self.scale))
                .transpose()?,
            ..camera
        };
        // The placed camera unrolled, and the picture's down the group gives it: the roll
        // turns one into the other.
        let unrolled = placed.view(canvas)?;
        let down = self.turn(own.down);
        let roll = dot(down, unrolled.right).atan2(dot(down, unrolled.down));
        Ok(SceneCamera {
            roll_deg: finite(roll.to_degrees())?,
            ..placed
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{GroupPose, angles_of, rotation_of};
    use crate::command::Vec2;
    use crate::finite::Finite32;
    use crate::geometry::{
        Canvas, CanvasPoint, FitMode, ItemGeometry, Placed, SceneCamera, Size, Vec3,
    };

    const CANVAS: Canvas = Canvas {
        width: 1920,
        height: 1080,
    };

    fn f(value: f32) -> Finite32 {
        Finite32::new(value).expect("finite")
    }

    fn boxed(x: f32, y: f32, w: f32, h: f32) -> ItemGeometry {
        ItemGeometry {
            center: Vec2 { x: f(x), y: f(y) },
            size: Size {
                width: f(w),
                height: f(h),
            },
            fit_mode: FitMode::Fit,
            rotation_deg: Finite32::ZERO,
            tilt_deg: Finite32::ZERO,
            turn_deg: Finite32::ZERO,
            depth_px: Finite32::ZERO,
            perspective_px: None,
            flip_h: false,
            flip_v: false,
            billboard: false,
            surface: None,
        }
    }

    /// The group placed over the whole canvas, unturned.
    fn neutral() -> ItemGeometry {
        boxed(960.0, 540.0, 1920.0, 1080.0)
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1e-3 * (1.0 + a.abs().max(b.abs()))
    }

    fn corners(geometry: ItemGeometry) -> Vec<[f64; 3]> {
        geometry.corners_in_scene().to_vec()
    }

    fn same_corners(a: ItemGeometry, b: ItemGeometry) {
        for (p, q) in corners(a).iter().zip(corners(b).iter()) {
            for i in 0..3 {
                assert!(
                    (p[i] - q[i]).abs() < 1e-2 * (1.0 + p[i].abs()),
                    "{p:?} against {q:?}"
                );
            }
        }
    }

    #[test]
    fn a_neutral_group_places_a_child_where_its_own_geometry_would() {
        let pose = GroupPose::of(neutral(), CANVAS).expect("pose");
        let child = ItemGeometry {
            rotation_deg: f(20.0),
            turn_deg: f(-35.0),
            tilt_deg: f(12.0),
            depth_px: f(-300.0),
            billboard: true,
            ..boxed(400.0, 200.0, 320.0, 180.0)
        };
        let placed = pose.place(child).expect("placed");
        assert!(close(placed.center.x.get(), 400.0));
        assert!(close(placed.center.y.get(), 200.0));
        assert!(close(placed.depth_px.get(), -300.0));
        assert!(close(placed.size.width.get(), 320.0));
        same_corners(placed, child);
        assert!(placed.billboard, "the child keeps what isn't its pose");
    }

    #[test]
    fn the_group_moves_scales_and_turns_its_children() {
        // Half the canvas's size, centred at (500, 300), pushed back and turned 90°.
        let group = ItemGeometry {
            turn_deg: f(90.0),
            depth_px: f(-200.0),
            ..boxed(500.0, 300.0, 960.0, 540.0)
        };
        let pose = GroupPose::of(group, CANVAS).expect("pose");
        assert!(close(pose.scale(), 0.5));
        // A child 200 px right of the group's centre.
        let child = boxed(1160.0, 540.0, 100.0, 100.0);
        let placed = pose.place(child).expect("placed");
        // Scaled to 100 px, then turned a quarter: +x goes to −z.
        assert!(close(placed.center.x.get(), 500.0), "{placed:?}");
        assert!(close(placed.center.y.get(), 300.0));
        assert!(close(placed.depth_px.get(), -300.0));
        assert!(close(placed.size.width.get(), 50.0));
        assert!(close(placed.turn_deg.get(), 90.0));
    }

    #[test]
    fn fit_modes_scale_uniformly() {
        let wide = boxed(960.0, 540.0, 1920.0, 540.0);
        for (fit_mode, scale) in [
            (FitMode::Fit, 0.5),
            (FitMode::Stretch, 0.5),
            (FitMode::Fill, 1.0),
        ] {
            let pose = GroupPose::of(ItemGeometry { fit_mode, ..wide }, CANVAS).expect("pose");
            assert!(close(pose.scale(), scale), "{fit_mode:?}");
        }
    }

    #[test]
    fn nested_groups_compose() {
        let outer = ItemGeometry {
            rotation_deg: f(30.0),
            tilt_deg: f(-20.0),
            ..boxed(800.0, 500.0, 960.0, 540.0)
        };
        let inner = ItemGeometry {
            turn_deg: f(45.0),
            depth_px: f(100.0),
            ..boxed(700.0, 400.0, 960.0, 540.0)
        };
        let child = ItemGeometry {
            rotation_deg: f(-10.0),
            ..boxed(300.0, 900.0, 200.0, 120.0)
        };
        let outer_pose = GroupPose::of(outer, CANVAS).expect("outer");
        let inner_pose =
            GroupPose::of(outer_pose.place(inner).expect("inner"), CANVAS).expect("pose");
        let placed = inner_pose.place(child).expect("child");
        // The same as placing each corner point by point through both.
        let inner_alone = GroupPose::of(inner, CANVAS).expect("inner alone");
        let by_points: Vec<[f64; 3]> = corners(child)
            .iter()
            .map(|corner| outer_pose.place_point(inner_alone.place_point(*corner)))
            .collect();
        for (p, q) in corners(placed).iter().zip(by_points.iter()) {
            for i in 0..3 {
                assert!((p[i] - q[i]).abs() < 1e-2, "{p:?} against {q:?}");
            }
        }
    }

    /// Two 45° turns add to a quarter turn, where the tilt and the rotation fold into one
    /// angle: the child still lands where placing its corners point by point puts them.
    #[test]
    fn a_composed_quarter_turn_keeps_the_childs_orientation() {
        for ((gr, gt, gx), (cr, ct, cx)) in [
            ((0.0, 45.0, 20.0), (15.0, 45.0, 0.0)),
            ((30.0, 45.0, 0.0), (0.0, 45.0, -25.0)),
            ((0.0, -45.0, 10.0), (-20.0, -45.0, 35.0)),
            ((0.0, 45.0, 0.0), (0.0, 45.0, 0.0)),
        ] {
            let group = ItemGeometry {
                rotation_deg: f(gr),
                turn_deg: f(gt),
                tilt_deg: f(gx),
                ..boxed(800.0, 500.0, 960.0, 540.0)
            };
            let child = ItemGeometry {
                rotation_deg: f(cr),
                turn_deg: f(ct),
                tilt_deg: f(cx),
                ..boxed(300.0, 900.0, 200.0, 120.0)
            };
            let pose = GroupPose::of(group, CANVAS).expect("pose");
            let placed = pose.place(child).expect("child");
            for (p, corner) in corners(placed).iter().zip(corners(child).iter()) {
                let q = pose.place_point(*corner);
                for i in 0..3 {
                    assert!((p[i] - q[i]).abs() < 1e-2, "{p:?} against {q:?}");
                }
            }
        }
    }

    #[test]
    fn angles_round_trip_through_a_rotation() {
        for (r, t, x) in [
            (0.0, 0.0, 0.0),
            (30.0, 40.0, -50.0),
            (-170.0, 80.0, 120.0),
            (10.0, -89.5, 33.0),
            (0.0, 90.0, 25.0),
            (0.0, -90.0, -60.0),
        ] {
            let m = rotation_of(r, t, x);
            let (r2, t2, x2) = angles_of(&m);
            let back = rotation_of(r2 as f32, t2 as f32, x2 as f32);
            for row in 0..3 {
                for col in 0..3 {
                    assert!(
                        (m[row][col] - back[row][col]).abs() < 1e-5,
                        "({r}, {t}, {x}) came back as ({r2}, {t2}, {x2})"
                    );
                }
            }
        }
    }

    fn camera(position: [f32; 3], target: [f32; 3]) -> SceneCamera {
        let vec3 = |[x, y, z]: [f32; 3]| Vec3::new(x, y, z).expect("finite");
        SceneCamera {
            position: vec3(position),
            target: vec3(target),
            ..SceneCamera::framing(CANVAS)
        }
    }

    /// Where `point`, in the scene's space, lands on the canvas through `camera`.
    fn seen(camera: SceneCamera, point: [f64; 3]) -> CanvasPoint {
        let size = 1.0;
        let probe = ItemGeometry {
            center: Vec2 {
                x: f(point[0] as f32),
                y: f(point[1] as f32),
            },
            depth_px: f(point[2] as f32),
            ..boxed(0.0, 0.0, size, size)
        };
        let view = camera.view(CANVAS).expect("view");
        let quad = probe.project_seen(Some(&view)).expect("projected");
        let [Placed::At(a), _, Placed::At(c), _] = quad.corners else {
            panic!("behind the camera");
        };
        CanvasPoint {
            x: (a.x + c.x) * 0.5,
            y: (a.y + c.y) * 0.5,
        }
    }

    #[test]
    fn a_camera_in_a_group_sees_the_group_as_it_would_unplaced() {
        let group = ItemGeometry {
            rotation_deg: f(25.0),
            turn_deg: f(-30.0),
            tilt_deg: f(15.0),
            depth_px: f(-400.0),
            ..boxed(700.0, 450.0, 960.0, 540.0)
        };
        let pose = GroupPose::of(group, CANVAS).expect("pose");
        let own = SceneCamera {
            roll_deg: f(10.0),
            focus_px: Some(f(800.0)),
            blur_px: f(4.0),
            ..camera([300.0, 400.0, 900.0], [1100.0, 500.0, -100.0])
        };
        let placed = pose.place_camera(own, CANVAS).expect("placed");
        assert!(close(placed.focus_px.expect("focus").get(), 400.0));
        // A point of the group's space looks the same through the placed camera, placed,
        // as through the camera's own numbers, unplaced.
        for point in [
            [960.0, 540.0, 0.0],
            [1200.0, 300.0, -250.0],
            [500.0, 800.0, 120.0],
        ] {
            let a = seen(own, point);
            let b = seen(placed, pose.place_point(point));
            assert!(close(a.x, b.x) && close(a.y, b.y), "{a:?} against {b:?}");
        }
    }
}
