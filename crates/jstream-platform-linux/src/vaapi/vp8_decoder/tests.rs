use super::Slots;
use crate::media::vp8::{Header, Parser, Update};

/// A header with the given updates, from the first frame of a real stream.
fn header(refresh_last: bool, golden: Update, alternate: Update) -> Header {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/media/testdata/vp8-altref.webm");
    let mut webm = crate::media::mkv::open(std::fs::File::open(path).expect("opens"))
        .expect("reads")
        .expect("webm");
    let track = webm.video.take().expect("video");
    let data = webm.read(&track, &track.samples[0]).expect("reads");
    let mut h = Parser::new().parse(&data).expect("parses");
    h.refresh_last = refresh_last;
    h.golden = golden;
    h.alternate = alternate;
    h
}

#[test]
fn a_key_frame_fills_every_slot() {
    let after = Slots::default().after(&header(true, Update::Current, Update::Current), 3);
    assert_eq!(
        after,
        Slots {
            last: Some(3),
            golden: Some(3),
            alternate: Some(3),
        }
    );
}

#[test]
fn copies_take_the_slots_as_they_were_before_the_frame() {
    let before = Slots {
        last: Some(0),
        golden: Some(1),
        alternate: Some(2),
    };
    // Golden takes the old alternate and alternate the old golden: a swap.
    let swapped = before.after(&header(true, Update::Other, Update::Other), 4);
    assert_eq!(
        swapped,
        Slots {
            last: Some(4),
            golden: Some(2),
            alternate: Some(1),
        }
    );
    // Both take the last frame from before this one, which this one replaces.
    let from_last = before.after(&header(true, Update::Last, Update::Last), 4);
    assert_eq!(
        from_last,
        Slots {
            last: Some(4),
            golden: Some(0),
            alternate: Some(0),
        }
    );
    // A hidden alternate reference: only alternate changes.
    let hidden = before.after(&header(false, Update::Keep, Update::Current), 4);
    assert_eq!(
        hidden,
        Slots {
            last: Some(0),
            golden: Some(1),
            alternate: Some(4),
        }
    );
    assert!(hidden.holds(4) && !hidden.holds(3));
}
