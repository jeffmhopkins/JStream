use super::*;

const ID: [f64; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

fn trs(translation: [f64; 3]) -> Rest {
    Rest::Trs {
        translation,
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0; 3],
    }
}

fn root() -> RigNode {
    RigNode {
        parent: None,
        rest: trs([0.0; 3]),
    }
}

fn slot(node: usize) -> Slot {
    Slot {
        node,
        inverse_bind: ID,
    }
}

fn clip(channels: Vec<Channel>) -> Clip {
    Clip {
        name: "walk".into(),
        channels,
        duration: 0.0,
    }
}

fn channel(path: Path, interpolation: Interpolation, times: &[f32], values: &[f32]) -> Channel {
    Channel {
        node: 0,
        path,
        interpolation,
        times: times.to_vec(),
        values: values.to_vec(),
    }
}

fn close(a: [f32; 3], b: [f32; 3]) {
    for i in 0..3 {
        assert!((a[i] - b[i]).abs() < 1e-5, "{a:?} != {b:?}");
    }
}

/// Where the origin goes under a slot's matrix.
fn origin(m: &Mat4) -> [f32; 3] {
    skin(std::slice::from_ref(m), [0; 4], [1.0, 0.0, 0.0, 0.0], [0.0; 3])
}

#[test]
fn a_linear_translation_is_halfway_at_half_time_and_flipped_to_y_down() {
    let move_up = channel(
        Path::Translation,
        Interpolation::Linear,
        &[0.0, 2.0],
        &[0.0, 0.0, 0.0, 4.0, 2.0, 0.0],
    );
    let rig = Rig::new(vec![root()], vec![slot(0)], vec![clip(vec![move_up])]).unwrap();
    assert_eq!(rig.clips()[0].duration, 2.0);
    // glTF's y up is the scene's y down.
    close(origin(&rig.pose(0, 1.0)[0]), [2.0, -1.0, 0.0]);
    // Before the first key and after the last, the ends hold.
    close(origin(&rig.pose(0, -1.0)[0]), [0.0, 0.0, 0.0]);
    close(origin(&rig.pose(0, 9.0)[0]), [4.0, -2.0, 0.0]);
    close(origin(&rig.rest()[0]), [0.0, 0.0, 0.0]);
}

#[test]
fn a_step_holds_each_key_until_the_next() {
    let jump = channel(
        Path::Translation,
        Interpolation::Step,
        &[0.0, 1.0],
        &[0.0, 0.0, 0.0, 3.0, 0.0, 0.0],
    );
    let rig = Rig::new(vec![root()], vec![slot(0)], vec![clip(vec![jump])]).unwrap();
    close(origin(&rig.pose(0, 0.99)[0]), [0.0, 0.0, 0.0]);
    close(origin(&rig.pose(0, 1.0)[0]), [3.0, 0.0, 0.0]);
}

#[test]
fn a_rotation_slerps_the_short_way() {
    // A quarter turn about z, from none.
    let h = std::f32::consts::FRAC_1_SQRT_2;
    let turn = channel(
        Path::Rotation,
        Interpolation::Linear,
        &[0.0, 1.0],
        &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, h, h],
    );
    let rig = Rig::new(vec![root()], vec![slot(0)], vec![clip(vec![turn])]).unwrap();
    let m = rig.pose(0, 0.5)[0];
    let x = skin(&[m], [0; 4], [1.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
    // An eighth of a turn about z takes x to (cos 45°, sin 45°) in glTF, y flipped.
    close(x, [h, -h, 0.0]);
}

#[test]
fn a_cubic_spline_with_flat_tangents_eases_through_the_middle() {
    let ease = channel(
        Path::Translation,
        Interpolation::CubicSpline,
        &[0.0, 1.0],
        // in-tangent, value, out-tangent for each key.
        &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    );
    let rig = Rig::new(vec![root()], vec![slot(0)], vec![clip(vec![ease])]).unwrap();
    close(origin(&rig.pose(0, 0.5)[0]), [1.0, 0.0, 0.0]);
    // Slower than linear near the start.
    assert!(origin(&rig.pose(0, 0.25)[0])[0] < 0.5);
}

#[test]
fn a_child_moves_with_its_parent_and_a_skin_slot_is_its_bind_offset() {
    let nodes = vec![
        root(),
        RigNode {
            parent: Some(0),
            rest: trs([0.0, 1.0, 0.0]),
        },
    ];
    // The joint's inverse bind undoes its rest place, so at rest its slot is the identity.
    let mut undo = ID;
    undo[13] = -1.0;
    let slots = vec![
        slot(1),
        Slot {
            node: 1,
            inverse_bind: undo,
        },
    ];
    let slide = channel(
        Path::Translation,
        Interpolation::Linear,
        &[0.0, 1.0],
        &[0.0, 0.0, 0.0, 5.0, 0.0, 0.0],
    );
    let rig = Rig::new(nodes, slots, vec![clip(vec![slide])]).unwrap();
    let rest = rig.rest();
    close(origin(&rest[0]), [0.0, -1.0, 0.0]);
    close(origin(&rest[1]), [0.0, 0.0, 0.0]);
    let posed = rig.pose(0, 1.0);
    close(origin(&posed[0]), [5.0, -1.0, 0.0]);
    close(origin(&posed[1]), [5.0, 0.0, 0.0]);
    // Two joints half each land halfway.
    let half = skin(&posed, [0, 1, 0, 0], [0.5, 0.5, 0.0, 0.0], [0.0; 3]);
    close(half, [5.0, -0.5, 0.0]);
    // No weight leaves a vertex where it is.
    close(skin(&posed, [0; 4], [0.0; 4], [1.0, 2.0, 3.0]), [1.0, 2.0, 3.0]);
}

#[test]
fn a_rig_refuses_a_parent_after_its_child_and_keys_that_do_not_match() {
    let late = vec![
        RigNode {
            parent: Some(1),
            rest: trs([0.0; 3]),
        },
        root(),
    ];
    assert_eq!(
        Rig::new(late, vec![], vec![]),
        Err(RigError::Parent { node: 0, parent: 1 })
    );
    let short = channel(Path::Translation, Interpolation::Linear, &[0.0, 1.0], &[0.0; 3]);
    assert!(matches!(
        Rig::new(vec![root()], vec![slot(0)], vec![clip(vec![short])]),
        Err(RigError::Keys { channel: 0, .. })
    ));
    let backwards = channel(Path::Scale, Interpolation::Step, &[1.0, 0.0], &[1.0; 6]);
    assert!(Rig::new(vec![root()], vec![slot(0)], vec![clip(vec![backwards])]).is_err());
    assert!(matches!(
        Rig::new(vec![root()], vec![slot(3)], vec![]),
        Err(RigError::Node { node: 3, .. })
    ));
}
