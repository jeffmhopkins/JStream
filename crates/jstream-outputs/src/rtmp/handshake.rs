//! The simple (unsigned) RTMP handshake: C0+C1 → S0+S1+S2 → C2.

use std::io::{Read, Write};

use super::RtmpError;

/// RTMP protocol version carried in C0 and S0.
pub(crate) const RTMP_VERSION: u8 = 3;
/// Length of C1, C2, S1 and S2.
pub(crate) const HANDSHAKE_SIZE: usize = 1536;

/// Builds C1: a 4-byte time (0), 4 zero bytes (which selects the simple handshake) and
/// 1528 bytes of filler from `seed`. The filler only has to be unpredictable enough for
/// the server to match S2 against it; it is not a secret.
pub(crate) fn c1(seed: u64) -> [u8; HANDSHAKE_SIZE] {
    let mut c1 = [0u8; HANDSHAKE_SIZE];
    // xorshift64*; a zero state would stay zero.
    let mut state = seed | 1;
    for chunk in c1[8..].chunks_mut(8) {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let word = state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_be_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
    }
    c1
}

/// Runs the client side of the handshake on `stream`.
///
/// Sends C0 and C1 together, reads S0 (which must be version 3) and S1, answers with C2
/// (an echo of S1) and reads S2. S2 is not checked against C1: servers differ in what
/// they echo, and a mismatch would not make the session unusable.
///
/// # Errors
/// [`RtmpError::HandshakeVersion`] when S0 is not 3, and I/O errors (or
/// [`RtmpError::ConnectionClosed`]) from the transport.
pub(crate) fn client<S: Read + Write>(stream: &mut S, seed: u64) -> Result<(), RtmpError> {
    let io = |e| RtmpError::io("handshaking", e);
    let mut c0c1 = Vec::with_capacity(1 + HANDSHAKE_SIZE);
    c0c1.push(RTMP_VERSION);
    c0c1.extend_from_slice(&c1(seed));
    stream.write_all(&c0c1).map_err(io)?;
    stream.flush().map_err(io)?;

    let mut s0 = [0u8; 1];
    stream.read_exact(&mut s0).map_err(io)?;
    if s0[0] != RTMP_VERSION {
        return Err(RtmpError::HandshakeVersion { version: s0[0] });
    }
    let mut s1 = [0u8; HANDSHAKE_SIZE];
    stream.read_exact(&mut s1).map_err(io)?;
    stream.write_all(&s1).map_err(io)?;
    stream.flush().map_err(io)?;
    let mut s2 = [0u8; HANDSHAKE_SIZE];
    stream.read_exact(&mut s2).map_err(io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtmp::test_util::Scripted;

    #[test]
    fn client_sends_c0_c1_then_echoes_s1() {
        let mut s1 = [0u8; HANDSHAKE_SIZE];
        for (i, b) in s1.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let mut input = vec![RTMP_VERSION];
        input.extend_from_slice(&s1);
        input.extend_from_slice(&c1(7)); // S2 echoing C1
        let mut stream = Scripted::new(input);
        client(&mut stream, 7).unwrap();

        let out = stream.written();
        assert_eq!(out.len(), 1 + 2 * HANDSHAKE_SIZE);
        assert_eq!(out[0], 3, "C0 is the version");
        let sent_c1 = &out[1..=HANDSHAKE_SIZE];
        assert_eq!(&sent_c1[..8], &[0; 8], "C1 time and zero fields");
        assert_eq!(sent_c1, c1(7).as_slice());
        assert!(sent_c1[8..].iter().any(|&b| b != 0), "C1 carries filler");
        assert_eq!(&out[1 + HANDSHAKE_SIZE..], s1.as_slice(), "C2 echoes S1");
        assert!(stream.fully_read());
    }

    #[test]
    fn filler_depends_on_seed() {
        assert_ne!(c1(1)[8..], c1(2)[8..]);
    }

    #[test]
    fn rejects_other_versions() {
        let mut stream = Scripted::new(vec![6; 1 + 2 * HANDSHAKE_SIZE]);
        assert!(matches!(
            client(&mut stream, 1),
            Err(RtmpError::HandshakeVersion { version: 6 })
        ));
    }

    #[test]
    fn truncated_reply_is_connection_closed() {
        let mut stream = Scripted::new(vec![RTMP_VERSION, 0, 0]);
        assert!(matches!(
            client(&mut stream, 1),
            Err(RtmpError::ConnectionClosed { .. })
        ));
    }
}
