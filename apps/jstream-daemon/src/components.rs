//! The OS components the live engine needs, checked at startup on Windows: Media
//! Foundation itself (`jstream_platform_windows::check_media_foundation`), then its AAC
//! encoder, which every stream needs for its audio. An N edition without the Media
//! Feature Pack lacks both; this module holds the AAC half, which is plain data in,
//! error out, so it is tested on every platform.

use jstream_media::Codec;
use jstream_platform::{CodecInfo, CodecRole, PlatformError};

/// `Ok` when `codecs` (what `Codecs::list` found) has an AAC encoder, else a
/// [`PlatformError::MissingComponent`] that says how to install it.
pub fn aac_encoder(codecs: &[CodecInfo]) -> Result<(), PlatformError> {
    if codecs
        .iter()
        .any(|codec| codec.codec == Codec::Aac && codec.role == CodecRole::AudioEncoder)
    {
        return Ok(());
    }
    Err(PlatformError::MissingComponent {
        component: "Media Foundation's AAC encoder",
        remedy: "Media Foundation has no AAC audio encoder, which every stream needs. On an \
                 N edition of Windows, install the Media Feature Pack (Settings > System > \
                 Optional features); on Windows Server, add the \"Server Media Foundation\" \
                 feature. Then restart and start JStream again."
            .to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::aac_encoder;
    use jstream_media::Codec;
    use jstream_platform::{CodecInfo, CodecRole, PlatformError};

    fn info(codec: Codec, role: CodecRole) -> CodecInfo {
        CodecInfo {
            name: "test codec".into(),
            codec,
            role,
            hardware: false,
        }
    }

    #[test]
    fn an_aac_encoder_passes() {
        let codecs = [
            info(Codec::H264, CodecRole::VideoEncoder),
            info(Codec::Aac, CodecRole::AudioEncoder),
        ];
        assert!(aac_encoder(&codecs).is_ok());
    }

    /// No AAC encoder, including when only H.264 is there: the error names the
    /// component and both ways to install it.
    #[test]
    fn a_missing_aac_encoder_says_what_to_install() {
        for codecs in [vec![], vec![info(Codec::H264, CodecRole::VideoEncoder)]] {
            match aac_encoder(&codecs) {
                Err(error @ PlatformError::MissingComponent { .. }) => {
                    let text = error.to_string();
                    assert!(
                        text.starts_with("Media Foundation's AAC encoder is not installed"),
                        "{text}"
                    );
                    assert!(text.contains("Media Feature Pack"), "{text}");
                    assert!(text.contains("Server Media Foundation"), "{text}");
                }
                other => panic!("expected MissingComponent, got {other:?}"),
            }
        }
    }
}
