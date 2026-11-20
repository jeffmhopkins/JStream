//! What an AAC stream's `AudioSpecificConfig` (ISO/IEC 14496-3 §1.6.2.1) says about
//! the tools it needs, so a stream this crate won't decode is refused by name
//! (ADR-0160). Linux decodes AAC-LC only, through symphonia: never SBR or parametric
//! stereo. A stream that signals HE-AAC explicitly is `Unsupported`; one that hides SBR
//! in an AAC-LC stream (implicit signalling) plays its LC core.

use jstream_platform::PlatformError;

use super::h264::bits::BitReader;

/// The `audioObjectType` of AAC-LC.
const AAC_LC: u32 = 2;
/// SBR (HE-AAC) and parametric stereo (HE-AAC v2).
const SBR: u32 = 5;
const PS: u32 = 29;
/// `syncExtensionType` of the backward-compatible extension signalling.
const SYNC_EXTENSION: u32 = 0x2B7;

/// `audioObjectType`, with its escape.
fn object_type(r: &mut BitReader<'_>) -> Option<u32> {
    let kind = r.bits(5).ok()?;
    if kind == 31 {
        return Some(32 + r.bits(6).ok()?);
    }
    Some(kind)
}

/// `samplingFrequencyIndex`, with its escape.
fn skip_frequency(r: &mut BitReader<'_>) -> Option<()> {
    if r.bits(4).ok()? == 15 {
        r.bits(24).ok()?;
    }
    Some(())
}

/// Refuses an `AudioSpecificConfig` that needs more than AAC-LC, naming what it needs.
/// A config too short to read is let through: the decoder refuses it if it must.
///
/// # Errors
/// [`PlatformError::Unsupported`] for HE-AAC signalled explicitly (object type 5 or 29,
/// or the backward-compatible extension with SBR present), another object type, a
/// program config element, or more than two channels.
pub(crate) fn check(asc: &[u8]) -> Result<(), PlatformError> {
    let refuse = |what: String| {
        Err(PlatformError::Unsupported {
            what: format!("{what} in a media file on Linux: AAC-LC plays"),
        })
    };
    let mut r = BitReader::new(asc);
    let Some(kind) = object_type(&mut r) else {
        return Ok(());
    };
    match kind {
        SBR => return refuse("HE-AAC (SBR)".into()),
        PS => return refuse("HE-AAC v2 (parametric stereo)".into()),
        AAC_LC => {}
        other => return refuse(format!("AAC object type {other}")),
    }
    if skip_frequency(&mut r).is_none() {
        return Ok(());
    }
    let Ok(channels) = r.bits(4) else {
        return Ok(());
    };
    match channels {
        0 => return refuse("AAC with a program config element".into()),
        1 | 2 => {}
        _ => return refuse("AAC with more than two channels".into()),
    }
    // GASpecificConfig: frameLengthFlag, dependsOnCoreCoder (and its delay),
    // extensionFlag (0 for AAC-LC).
    let explicit_sbr = (|| -> Option<bool> {
        r.bits(1).ok()?;
        if r.flag().ok()? {
            r.bits(14).ok()?;
        }
        r.bits(1).ok()?;
        // The backward-compatible extension, when the config goes on (§1.6.6.2).
        if r.bits(11).ok()? != SYNC_EXTENSION {
            return Some(false);
        }
        if object_type(&mut r)? != SBR {
            return Some(false);
        }
        r.flag().ok()
    })();
    if explicit_sbr == Some(true) {
        return refuse("HE-AAC (SBR, signalled explicitly)".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check;
    use jstream_platform::PlatformError;

    fn refused(asc: &[u8]) -> String {
        match check(asc) {
            Err(PlatformError::Unsupported { what }) => what,
            other => panic!("{asc:02x?}: {other:?}"),
        }
    }

    #[test]
    fn aac_lc_mono_and_stereo_pass() {
        // AAC-LC, 44.1 kHz, stereo; and 48 kHz mono.
        check(&[0x12, 0x10]).expect("stereo");
        check(&[0x11, 0x88]).expect("mono");
        // Implicit HE-AAC is plain AAC-LC at the core rate (22.05 kHz).
        check(&[0x13, 0x90]).expect("implicit");
    }

    #[test]
    fn explicit_he_aac_is_refused_by_name() {
        // Object type 5 (SBR), 22.05 kHz core, stereo, 44.1 kHz output, then AAC-LC.
        assert!(refused(&[0x2B, 0x92, 0x08, 0x00]).contains("HE-AAC (SBR)"));
        // Object type 29 (PS).
        assert!(refused(&[0xEB, 0x92, 0x08, 0x00]).contains("HE-AAC v2"));
        // AAC-LC with the backward-compatible extension: 0x2B7, SBR, present, 44.1 kHz.
        assert!(refused(&[0x13, 0x90, 0x56, 0xE5, 0x98]).contains("signalled explicitly"));
        // The same extension with SBR absent passes.
        check(&[0x13, 0x90, 0x56, 0xE5, 0x00]).expect("SBR absent");
    }

    #[test]
    fn other_profiles_and_layouts_are_refused_by_name() {
        // AAC Main (1) and LTP (4).
        assert!(refused(&[0x0A, 0x10]).contains("object type 1"));
        assert!(refused(&[0x22, 0x10]).contains("object type 4"));
        // 5.1 (channel configuration 6), and a program config element (0).
        assert!(refused(&[0x11, 0xB0]).contains("more than two channels"));
        assert!(refused(&[0x12, 0x00]).contains("program config element"));
    }

    #[test]
    fn a_short_config_is_left_to_the_decoder() {
        check(&[]).expect("empty");
        check(&[0x12]).expect("one byte");
    }
}
