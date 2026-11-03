//! `codec.downloads`, `codec.download` and `codec.delete` (protocol 0.68, ADR-0061): the
//! codec libraries the platform downloads on a person's request, which on Linux is
//! Cisco's OpenH264. The engine answers them from its codec backend
//! ([`Codecs::downloads`]) with no engine lock held, since a download blocks for as long
//! as it takes. No MCP tool, plugin or hotkey carries them: they are a person's calls.

use std::sync::Arc;

use jstream_platform::media::Codec;
use jstream_platform::{CodecRole, Codecs, PlatformError};
use jstream_protocol::{
    Action, Call, CallResult, CodecDownloadInfo, ProtocolError, Query, QueryResult, VideoCodec,
};

/// The answer to `call` when it is one of the codec calls, with `codecs` the engine's
/// codec backend (`None` without media backends), or `None` for any other call.
pub(crate) fn call(
    codecs: Option<Arc<dyn Codecs>>,
    call: &Call,
) -> Option<Result<CallResult, ProtocolError>> {
    match call {
        Call::Query {
            query: Query::CodecDownloads,
        } => Some(Ok(CallResult::Query {
            result: QueryResult::CodecDownloads {
                downloads: codecs.as_deref().map(list).unwrap_or_default(),
            },
        })),
        Call::Action {
            action: Action::CodecDownload { download_id },
        } => Some(act(codecs.as_deref(), download_id, Op::Download)),
        Call::Action {
            action: Action::CodecDelete { download_id },
        } => Some(act(codecs.as_deref(), download_id, Op::Delete)),
        _ => None,
    }
}

/// What `codec.downloads` lists: each download the backend offers, ordered by ID, with
/// whether the machine has a hardware encoder for its codec.
fn list(codecs: &dyn Codecs) -> Vec<CodecDownloadInfo> {
    // A backend that can't list its codecs has no hardware encoder to name.
    let encoders = codecs.list().unwrap_or_default();
    let mut downloads: Vec<CodecDownloadInfo> = codecs
        .downloads()
        .into_iter()
        .filter_map(|download| {
            let codec = video_codec(download.codec)?;
            let hardware_encoder = encoders.iter().any(|e| {
                e.role == CodecRole::VideoEncoder && e.codec == download.codec && e.hardware
            });
            Some(CodecDownloadInfo {
                download_id: download.id.to_owned(),
                name: download.name.to_owned(),
                codec,
                version: download.version,
                notice: download.notice.to_owned(),
                license_url: download.license_url.to_owned(),
                license_text: download.license_text.to_owned(),
                installed: download.installed,
                hardware_encoder,
            })
        })
        .collect();
    downloads.sort_by(|a, b| a.download_id.cmp(&b.download_id));
    downloads
}

/// The protocol's name for a video codec a download encodes, or `None` for one the
/// protocol has no name for, which is then not listed.
fn video_codec(codec: Codec) -> Option<VideoCodec> {
    match codec {
        Codec::H264 => Some(VideoCodec::H264),
        Codec::Hevc => Some(VideoCodec::Hevc),
        Codec::Av1 => Some(VideoCodec::Av1),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Op {
    Download,
    Delete,
}

/// `codec.download` or `codec.delete` of `download_id`.
fn act(
    codecs: Option<&dyn Codecs>,
    download_id: &str,
    op: Op,
) -> Result<CallResult, ProtocolError> {
    let listed = codecs.is_some_and(|c| c.downloads().iter().any(|d| d.id == download_id));
    let (Some(codecs), true) = (codecs, listed) else {
        return Err(ProtocolError::invalid_argument(
            "download_id",
            format!("`{download_id}` is not a codec this engine downloads (see `codec.downloads`)"),
        ));
    };
    let (result, verb) = match op {
        Op::Download => (codecs.download(download_id), "downloading"),
        Op::Delete => (codecs.delete_download(download_id), "deleting"),
    };
    match result {
        Ok(()) => {
            tracing::info!(download_id, "codec {verb} done");
            Ok(CallResult::action())
        }
        Err(error) => {
            tracing::warn!(download_id, %error, "codec {verb} failed");
            Err(ProtocolError::ActionFailed {
                reason: failure(verb, download_id, &error),
            })
        }
    }
}

/// The reason `action_failed` gives: the platform's error, which names what failed.
fn failure(verb: &str, download_id: &str, error: &PlatformError) -> String {
    format!("{verb} `{download_id}` failed: {error}")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use jstream_platform::media::Codec;
    use jstream_platform::{
        AudioEncoder, AudioEncoderConfig, CodecDownload, CodecInfo, CodecRole, Codecs,
        PlatformError, VideoDecoder, VideoDecoderConfig, VideoEncoder, VideoEncoderConfig,
    };
    use jstream_protocol::{
        Action, Call, CallResult, ProtocolError, Query, QueryResult, VideoCodec,
    };

    /// A backend offering one download, with a hardware H.264 encoder or not, and a
    /// download that fails or succeeds.
    struct FakeCodecs {
        hardware: bool,
        installed: Mutex<bool>,
        fails: bool,
    }

    impl FakeCodecs {
        fn shared(hardware: bool, fails: bool) -> Arc<dyn Codecs> {
            Arc::new(FakeCodecs {
                hardware,
                installed: Mutex::new(false),
                fails,
            })
        }
    }

    fn unsupported() -> PlatformError {
        PlatformError::Unsupported {
            what: "anything".into(),
        }
    }

    impl Codecs for FakeCodecs {
        fn list(&self) -> Result<Vec<CodecInfo>, PlatformError> {
            Ok(vec![CodecInfo {
                name: "an encoder".into(),
                codec: Codec::H264,
                role: CodecRole::VideoEncoder,
                hardware: self.hardware,
            }])
        }
        fn video_encoder(
            &self,
            _: &VideoEncoderConfig,
        ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
            Err(unsupported())
        }
        fn audio_encoder(
            &self,
            _: &AudioEncoderConfig,
        ) -> Result<Box<dyn AudioEncoder>, PlatformError> {
            Err(unsupported())
        }
        fn video_decoder(
            &self,
            _: &VideoDecoderConfig,
        ) -> Result<Box<dyn VideoDecoder>, PlatformError> {
            Err(unsupported())
        }
        fn downloads(&self) -> Vec<CodecDownload> {
            vec![CodecDownload {
                id: "openh264",
                name: "Cisco's OpenH264 (software H.264)",
                codec: Codec::H264,
                version: "2.6.0".into(),
                notice: "OpenH264 Video Codec provided by Cisco Systems, Inc.",
                license_url: "https://www.openh264.org/BINARY_LICENSE.txt",
                license_text: "the licence",
                installed: *self.installed.lock().expect("lock"),
            }]
        }
        fn download(&self, _: &str) -> Result<(), PlatformError> {
            if self.fails {
                return Err(PlatformError::Os {
                    call: "checking the OpenH264 download's SHA-256",
                    code: None,
                    message: "not the pinned one; it was discarded".into(),
                });
            }
            *self.installed.lock().expect("lock") = true;
            Ok(())
        }
        fn delete_download(&self, _: &str) -> Result<(), PlatformError> {
            *self.installed.lock().expect("lock") = false;
            Ok(())
        }
    }

    fn listed(codecs: &Arc<dyn Codecs>) -> Vec<jstream_protocol::CodecDownloadInfo> {
        let call = Call::Query {
            query: Query::CodecDownloads,
        };
        match super::call(Some(Arc::clone(codecs)), &call) {
            Some(Ok(CallResult::Query {
                result: QueryResult::CodecDownloads { downloads },
            })) => downloads,
            other => panic!("not a list: {other:?}"),
        }
    }

    fn action(codecs: Option<&Arc<dyn Codecs>>, action: Action) -> Result<(), ProtocolError> {
        super::call(codecs.map(Arc::clone), &Call::Action { action })
            .expect("a codec call")
            .map(|_| ())
    }

    fn download(id: &str) -> Action {
        Action::CodecDownload {
            download_id: id.into(),
        }
    }

    #[test]
    fn lists_downloads_and_deletes() {
        let codecs = FakeCodecs::shared(false, false);
        let list = listed(&codecs);
        assert_eq!(list.len(), 1);
        let info = &list[0];
        assert_eq!(info.download_id, "openh264");
        assert_eq!(info.codec, VideoCodec::H264);
        assert_eq!(
            info.notice,
            "OpenH264 Video Codec provided by Cisco Systems, Inc."
        );
        assert!(!info.installed && !info.hardware_encoder);
        action(Some(&codecs), download("openh264")).expect("downloads");
        assert!(listed(&codecs)[0].installed);
        action(
            Some(&codecs),
            Action::CodecDelete {
                download_id: "openh264".into(),
            },
        )
        .expect("deletes");
        assert!(!listed(&codecs)[0].installed);
    }

    #[test]
    fn says_whether_the_machine_has_a_hardware_encoder() {
        assert!(listed(&FakeCodecs::shared(true, false))[0].hardware_encoder);
    }

    #[test]
    fn refuses_an_unknown_id_and_reports_a_failed_download() {
        let codecs = FakeCodecs::shared(false, true);
        let Err(ProtocolError::InvalidArgument { field, .. }) =
            action(Some(&codecs), download("x264"))
        else {
            panic!("an unknown ID is refused");
        };
        assert_eq!(field, "download_id");
        let Err(ProtocolError::InvalidArgument { field, .. }) = action(None, download("openh264"))
        else {
            panic!("an engine with no codec backend downloads nothing");
        };
        assert_eq!(field, "download_id");
        let Err(ProtocolError::ActionFailed { reason }) =
            action(Some(&codecs), download("openh264"))
        else {
            panic!("a failed download is action_failed");
        };
        assert!(reason.contains("discarded"), "{reason}");
        assert!(!listed(&codecs)[0].installed);
    }

    #[test]
    fn an_engine_without_codecs_lists_none() {
        let call = Call::Query {
            query: Query::CodecDownloads,
        };
        let Some(Ok(CallResult::Query {
            result: QueryResult::CodecDownloads { downloads },
        })) = super::call(None, &call)
        else {
            panic!("a list");
        };
        assert!(downloads.is_empty());
        assert!(
            super::call(
                None,
                &Call::Query {
                    query: Query::SceneList
                }
            )
            .is_none()
        );
    }
}
