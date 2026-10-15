//! H.264 framing and the decoder configuration record, from encoder output.
//!
//! Nothing may panic. Annex B converted to AVCC must split into the same NAL units, and
//! back to Annex B into the same ones again.

#![no_main]

use jstream_mux::h264::{
    AvcDecoderConfig, annexb_nalus, annexb_to_avcc, avcc_nalus, avcc_to_annexb,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = avcc_to_annexb(data);
    if let Ok(config) = AvcDecoderConfig::from_annexb(data) {
        let _ = config.to_bytes();
    }
    let Ok(nalus) = annexb_nalus(data) else {
        return;
    };
    let avcc = annexb_to_avcc(data).expect("split Annex B converts");
    assert_eq!(avcc_nalus(&avcc).expect("AVCC splits"), nalus);
    let annexb = avcc_to_annexb(&avcc).expect("AVCC converts back");
    assert_eq!(annexb_nalus(&annexb).expect("Annex B splits"), nalus);
});
