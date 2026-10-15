//! AMF0 decode, as RTMP command messages from a server arrive.
//!
//! Decoding must never panic. When what decodes also encodes, encoding is a fixed point:
//! decoding the encoded bytes and encoding again gives the same bytes. (Bytes are
//! compared rather than values, because a NaN number is not equal to itself.)

#![no_main]

use jstream_mux::amf0::Amf0Value;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut input = data;
    let _ = Amf0Value::decode(&mut input);

    let Ok(values) = Amf0Value::decode_all(data) else {
        return;
    };
    let Ok(encoded) = Amf0Value::encode_all(&values) else {
        return;
    };
    let again = Amf0Value::decode_all(&encoded)
        .unwrap_or_else(|e| panic!("encoded values don't decode: {e}"));
    let reencoded = Amf0Value::encode_all(&again).expect("decoded values encode");
    assert_eq!(reencoded, encoded);
});
