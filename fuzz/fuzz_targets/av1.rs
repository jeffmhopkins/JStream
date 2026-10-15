//! AV1 OBU parsing and the codec configuration record, from encoder output.
//!
//! Nothing may panic. An OBU rebuilt with its size field must parse as the same OBU, and
//! a temporal unit converted to a sample must split into the same OBUs, less temporal
//! delimiters (type 2) and padding (type 15).

#![no_main]

use jstream_mux::av1::{Av1CodecConfig, Obu, obus, to_sample};
use libfuzzer_sys::fuzz_target;

/// What must survive a rewrite: the type and the payload.
fn content<'a>(obu: &Obu<'a>) -> (u8, &'a [u8]) {
    (obu.obu_type, obu.payload)
}

fuzz_target!(|data: &[u8]| {
    if let Ok(config) = Av1CodecConfig::from_obus(data) {
        let _ = config.to_bytes();
    }
    let Ok(units) = obus(data) else {
        return;
    };
    for obu in &units {
        let sized = obu.with_size_field();
        let parsed = obus(&sized).expect("a sized OBU parses");
        assert_eq!(parsed.iter().map(content).collect::<Vec<_>>(), [content(obu)]);
    }
    if let Ok(sample) = to_sample(data) {
        let kept: Vec<_> = units
            .iter()
            .filter(|o| !matches!(o.obu_type, 2 | 15))
            .map(content)
            .collect();
        let parsed = obus(&sample).expect("a sample splits into OBUs");
        assert_eq!(parsed.iter().map(content).collect::<Vec<_>>(), kept);
    }
});
