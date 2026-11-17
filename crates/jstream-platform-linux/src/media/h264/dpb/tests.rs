use super::super::{AvcConfig, split_length_prefixed};
use super::{Output, Planner};
use crate::media::mp4::{Codec, tests::open};

/// Plans every frame of a test file, checking each picture as it goes; returns the
/// output in order, and for each decoded picture its slice type and list lengths.
fn plan(name: &str) -> (Vec<Output>, Vec<(char, usize, usize)>) {
    let mut mp4 = open(name);
    let video = mp4.video.clone().expect("video");
    let Codec::Avc(record) = &video.codec else {
        panic!("not H.264");
    };
    let config = AvcConfig::parse(record).expect("avcC");
    let mut planner = Planner::new(18);
    for set in &config.parameter_sets {
        planner.parameter_set(set).expect("parameter set");
    }
    let slots = Planner::slots_for(planner.first_sps().expect("an SPS"));
    planner.slots = slots;
    let mut outputs = Vec::new();
    let mut pictures = Vec::new();
    for sample in &video.samples {
        let data = mp4.read(sample).expect("reads");
        let nals = split_length_prefixed(&data, config.length_size);
        let (before, picture) = planner.submit(&nals, sample.pts).expect("plans");
        outputs.extend(before);
        let picture = picture.expect("a picture");
        assert!(picture.slot < slots);
        // A picture never decodes into a slot a reference or a waiting frame holds.
        assert!(
            !picture
                .references
                .iter()
                .any(|f| f.slot == Some(picture.slot)),
            "slot reused"
        );
        for slice in &picture.slices {
            let kind = match slice.header.slice_type {
                super::SliceType::I => 'I',
                super::SliceType::P => 'P',
                super::SliceType::B => 'B',
                _ => '?',
            };
            let active0 = usize::from(slice.header.num_ref_idx_l0_active_minus1) + 1;
            let active1 = usize::from(slice.header.num_ref_idx_l1_active_minus1) + 1;
            match kind {
                'I' => assert!(slice.list0.is_empty() && slice.list1.is_empty()),
                'P' => {
                    assert_eq!(slice.list0.len(), active0);
                    assert!(slice.list1.is_empty());
                }
                _ => {
                    assert_eq!(slice.list0.len(), active0);
                    assert_eq!(slice.list1.len(), active1);
                    // Before the current picture in list 0, after it in list 1.
                    assert!(slice.list0[0].poc() < picture.current.poc());
                    assert!(slice.list1[0].poc() > picture.current.poc());
                }
            }
            for f in slice.list0.iter().chain(&slice.list1) {
                assert!(f.slot.is_some() && f.reference);
                assert!(picture.references.contains(f));
            }
            pictures.push((kind, slice.list0.len(), slice.list1.len()));
        }
        outputs.extend(planner.finish());
    }
    outputs.extend(planner.drain());
    (outputs, pictures)
}

fn assert_in_order(outputs: &[Output], count: usize) {
    assert_eq!(outputs.len(), count, "{outputs:?}");
    assert!(
        outputs.windows(2).all(|w| w[0].pts < w[1].pts),
        "out of order: {outputs:?}"
    );
}

#[test]
fn plans_high_profile_with_a_b_pyramid() {
    let (outputs, pictures) = plan("h264-high.mp4");
    assert_in_order(&outputs, 30);
    assert!(pictures.iter().any(|p| p.0 == 'B'));
    // Several references are in use.
    assert!(pictures.iter().any(|p| p.1 > 1), "{pictures:?}");
}

#[test]
fn plans_baseline_in_decoding_order() {
    let (outputs, pictures) = plan("h264-baseline.mov");
    assert_in_order(&outputs, 15);
    assert!(pictures.iter().all(|p| p.0 != 'B'));
}

#[test]
fn a_reset_skips_to_the_next_idr() {
    let mut mp4 = open("h264-high.mp4");
    let video = mp4.video.clone().expect("video");
    let Codec::Avc(record) = &video.codec else {
        panic!("not H.264");
    };
    let config = AvcConfig::parse(record).expect("avcC");
    let mut planner = Planner::new(18);
    for set in &config.parameter_sets {
        planner.parameter_set(set).expect("parameter set");
    }
    planner.reset();
    // Starting mid-GOP: nothing decodes until the IDR at sample 15.
    for (i, sample) in video.samples.iter().enumerate().skip(3) {
        let data = mp4.read(sample).expect("reads");
        let nals = split_length_prefixed(&data, config.length_size);
        let (_, picture) = planner.submit(&nals, sample.pts).expect("plans");
        assert_eq!(picture.is_some(), i >= 15, "sample {i}");
        planner.finish();
    }
}
