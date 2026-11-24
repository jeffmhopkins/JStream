//! NVENC (stub, replaced by the real session).

#[cfg(windows)]
pub(crate) use session::NvencEncoder;

#[cfg(windows)]
mod session {
    use jstream_platform::media::Packet;
    use jstream_platform::{
        CodecInfo, PlatformError, VideoEncoder, VideoEncoderConfig, VideoSurface,
    };

    use crate::gpu::EncodeGpu;

    /// A NVENC session.
    pub(crate) struct NvencEncoder {
        info: CodecInfo,
    }

    impl NvencEncoder {
        /// Opens a session.
        pub(crate) fn open(
            config: &VideoEncoderConfig,
            gpu: &EncodeGpu,
        ) -> Result<Self, PlatformError> {
            let _ = (config, gpu);
            Err(PlatformError::Unsupported {
                what: "NVENC is not written yet".into(),
            })
        }
    }

    impl VideoEncoder for NvencEncoder {
        fn info(&self) -> &CodecInfo {
            &self.info
        }
        fn can_accept(&self) -> bool {
            false
        }
        fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
            let _ = frame;
            Err(PlatformError::Busy { what: "stub" })
        }
        fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            let _ = out;
            Ok(())
        }
        fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            let _ = out;
            Ok(())
        }
    }
}
