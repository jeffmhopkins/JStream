//! The offer to download a codec the engine can't ship (ADR-0061): on Linux, Cisco's
//! OpenH264, the software H.264 encoder. Cisco's patent licence covers only Cisco's own
//! binary, downloaded straight to the machine, so the engine fetches it from Cisco's
//! server when a person asks, and never otherwise.
//!
//! - **When it is offered.** `codec.downloads` lists what the engine can download. The
//!   Outputs panel's Encoders section offers one that isn't installed when the machine
//!   has no hardware encoder for its codec, or when a profile asks for that codec in
//!   software (`hardware: avoid`). The encoder form offers it too, under the hardware
//!   choice, while the profile it edits would need it ([`needed_by_draft`]).
//! - **What it says.** Every offer names Cisco, the release, and shows Cisco's notice
//!   ("OpenH264 Video Codec provided by Cisco Systems, Inc.") with a link to Cisco's
//!   licence, as Cisco's condition 3 asks where the person controls the codec, and a
//!   folded "Licence terms" with the licence's full text, as its condition 4 asks where
//!   licensing information is presented. Once installed, the section keeps the notice,
//!   link and terms beside a Delete button, which with Download is how the person
//!   disables and re-enables it (condition 2).
//! - **The calls.** The Download button is the person's explicit request: it sends
//!   `codec.download`, which answers once the file is in place, and the button reads
//!   "Downloading…" until then. Delete sends `codec.delete`. Each re-reads the list
//!   after, and shows a failure, such as a download that didn't match JStream's pin, in
//!   the section. Agents can't make these calls (no MCP tool carries them), so the offer
//!   is only ever acted on by a person.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/adr/0061-source-types-and-decoding-scope-v2.md

use crate::app::SessionContext;
use jstream_control::Control;
use jstream_control::protocol::{
    Action, Call, CallResult, CodecDownloadInfo, EncoderProfileInfo, HardwarePreference, Query,
    QueryResult, VideoCodec,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// Why the section offers `download`, or `None` when it doesn't: it is installed, or
/// nothing on this machine needs it.
pub fn offer_reason(
    download: &CodecDownloadInfo,
    encoders: &[EncoderProfileInfo],
) -> Option<String> {
    if download.installed {
        return None;
    }
    let codec = codec_label(download.codec);
    if !download.hardware_encoder {
        return Some(format!(
            "This machine has no hardware {codec} encoder, so streaming and recording in \
             {codec} need {}.",
            download.name
        ));
    }
    let software: Vec<&str> = encoders
        .iter()
        .filter(|e| {
            e.profile.video.codec == download.codec
                && e.profile.video.hardware == HardwarePreference::Avoid
        })
        .map(|e| e.name.as_str())
        .collect();
    (!software.is_empty()).then(|| {
        format!(
            "{} {} software {codec}, which needs {}.",
            software.join(", "),
            if software.len() == 1 {
                "asks for"
            } else {
                "ask for"
            },
            download.name
        )
    })
}

/// Whether a profile encoding `codec` with `hardware` would need `download`: the codec
/// is the download's, it isn't installed, and the profile asks for software, or prefers
/// hardware on a machine that has none.
pub fn needed_by_draft(
    download: &CodecDownloadInfo,
    codec: VideoCodec,
    hardware: HardwarePreference,
) -> bool {
    codec == download.codec
        && !download.installed
        && match hardware {
            HardwarePreference::Avoid => true,
            HardwarePreference::Prefer => !download.hardware_encoder,
            _ => false,
        }
}

/// The codec as people read it.
fn codec_label(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "H.264",
        VideoCodec::Hevc => "HEVC",
        VideoCodec::Av1 => "AV1",
        _ => "video",
    }
}

/// What the Download button says: what it downloads, and whose it is.
pub fn download_label(download: &CodecDownloadInfo) -> String {
    format!("Download {} {}", short_name(download), download.version)
}

/// The download's name without its description: "Cisco's OpenH264".
fn short_name(download: &CodecDownloadInfo) -> &str {
    download
        .name
        .split_once(" (")
        .map_or(download.name.as_str(), |(name, _)| name)
}

/// The codec downloads `codec.downloads` lists. An engine that doesn't know the query
/// (older than 0.69) lists none, so the offer just doesn't show.
pub async fn list(control: &dyn Control) -> Result<Vec<CodecDownloadInfo>, String> {
    let call = Call::Query {
        query: Query::CodecDownloads,
    };
    match control.call(call).await.map_err(|e| e.to_string())?.result {
        CallResult::Query {
            result: QueryResult::CodecDownloads { downloads },
        } => Ok(downloads),
        _ => Err("the engine answered `codec.downloads` with something else".into()),
    }
}

/// The section's shared state: the list, the download in progress and the last
/// failure. The Outputs panel's Encoders section and the encoder form read it.
#[derive(Clone, Copy)]
pub struct CodecDownloads {
    /// What `codec.downloads` last listed.
    pub downloads: RwSignal<Vec<CodecDownloadInfo>>,
    /// The download a call is running for.
    busy: RwSignal<Option<String>>,
    /// The last call's failure.
    problem: RwSignal<Option<String>>,
    session: Option<SessionContext>,
}

impl CodecDownloads {
    /// The state, read from the engine once now.
    pub fn load() -> CodecDownloads {
        let state = CodecDownloads {
            downloads: RwSignal::new(Vec::new()),
            busy: RwSignal::new(None),
            problem: RwSignal::new(None),
            session: use_context::<SessionContext>(),
        };
        state.reload();
        state
    }

    /// Re-reads the list. A failure leaves it as it was: the offer is extra, and the
    /// panel's own reads report a broken session.
    fn reload(self) {
        let Some(session) = self.session.and_then(|s| s.get()) else {
            return;
        };
        spawn_local(async move {
            if let Ok(downloads) = list(&*session.control).await {
                self.downloads.try_set(downloads);
            }
        });
    }

    /// Sends `action` for `download_id`, then re-reads the list.
    fn run(self, download_id: String, action: Action) {
        let Some(session) = self.session.and_then(|s| s.get()) else {
            return;
        };
        if self.busy.get_untracked().is_some() {
            return;
        }
        self.busy.set(Some(download_id));
        self.problem.set(None);
        spawn_local(async move {
            let result = session.control.call(Call::Action { action }).await;
            self.busy.try_set(None);
            if let Err(error) = result {
                self.problem.try_set(Some(error.to_string()));
            }
            self.reload();
        });
    }

    /// Downloads `download_id`: the person pressed its Download button.
    pub fn download(self, download_id: String) {
        let action = Action::CodecDownload {
            download_id: download_id.clone(),
        };
        self.run(download_id, action);
    }

    /// Deletes `download_id`.
    pub fn delete(self, download_id: String) {
        let action = Action::CodecDelete {
            download_id: download_id.clone(),
        };
        self.run(download_id, action);
    }

    /// The listed download a profile encoding `codec` with `hardware` would need.
    pub fn needed_by(
        self,
        codec: VideoCodec,
        hardware: HardwarePreference,
    ) -> Option<CodecDownloadInfo> {
        self.downloads.with(|downloads| {
            downloads
                .iter()
                .find(|d| needed_by_draft(d, codec, hardware))
                .cloned()
        })
    }
}

/// Cisco's notice and the link to its licence, which go with every offer and with the
/// installed codec.
#[component]
fn Notice(download: CodecDownloadInfo) -> impl IntoView {
    view! {
        <span class="codec-notice">
            {download.notice.clone()}
            " "
            <a href=download.license_url.clone() target="_blank" rel="noopener noreferrer">
                "Cisco's licence"
            </a>
        </span>
    }
}

/// The licence's full text, folded: Cisco's condition 4 asks for all of it wherever the
/// app presents licensing information for the codec.
#[component]
fn Terms(download: CodecDownloadInfo) -> impl IntoView {
    view! {
        <details class="codec-terms">
            <summary>"Licence terms"</summary>
            <pre>{download.license_text.clone()}</pre>
        </details>
    }
}

/// The offer for `download`: what it is, why, Cisco's notice, and its Download button.
#[component]
pub(crate) fn Offer(
    state: CodecDownloads,
    download: CodecDownloadInfo,
    reason: String,
) -> impl IntoView {
    let id = download.download_id.clone();
    let busy = {
        let id = id.clone();
        move || state.busy.with(|b| b.as_deref() == Some(id.as_str()))
    };
    let label = download_label(&download);
    view! {
        <div class="codec-offer" role="group" aria-label=format!("Download {}", download.name)>
            <p>
                {reason}
                " JStream downloads Cisco's own copy straight from Cisco, only when you ask."
            </p>
            <p><Notice download=download.clone() /></p>
            <Terms download=download.clone() />
            <button
                type="button"
                class="primary"
                disabled=move || state.busy.with(Option::is_some)
                on:click=move |_| state.download(id.clone())
            >
                {move || if busy() { "Downloading…".to_owned() } else { label.clone() }}
            </button>
        </div>
    }
}

/// The downloads for the Encoders section: an offer for each one something needs, and
/// each installed one with its notice and a Delete button.
#[component]
pub(crate) fn Downloads(
    state: CodecDownloads,
    encoders: RwSignal<Vec<EncoderProfileInfo>>,
) -> impl IntoView {
    let shown = move || {
        state.downloads.with(|downloads| {
            encoders.with(|encoders| {
                downloads
                    .iter()
                    .filter_map(|d| {
                        let reason = offer_reason(d, encoders);
                        (d.installed || reason.is_some()).then(|| (d.clone(), reason))
                    })
                    .collect::<Vec<_>>()
            })
        })
    };
    view! {
        <div class="codec-downloads">
            <For
                each=shown
                key=|(d, reason)| (d.download_id.clone(), d.installed, reason.clone())
                let:shown
            >
                {
                    let (download, reason) = shown;
                    match reason {
                        Some(reason) => {
                            view! { <Offer state=state download=download reason=reason /> }.into_any()
                        }
                        None => {
                            let id = download.download_id.clone();
                            let name = format!("{} {}", download.name, download.version);
                            let terms = download.clone();
                            view! {
                                <div class="codec-installed">
                                    <span>{format!("{name} is installed.")}</span>
                                    " "
                                    <Notice download=download.clone() />
                                    " "
                                    <button
                                        type="button"
                                        aria-label=format!("Delete {}", download.name)
                                        disabled=move || state.busy.with(Option::is_some)
                                        on:click=move |_| state.delete(id.clone())
                                    >
                                        "Delete"
                                    </button>
                                    <Terms download=terms />
                                </div>
                            }
                                .into_any()
                        }
                    }
                }
            </For>
            {move || state.problem.get().map(|p| view! { <p class="problem" role="alert">{p}</p> })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::{download_label, needed_by_draft, offer_reason};
    use jstream_control::protocol::{
        CodecDownloadInfo, EncoderProfile, EncoderProfileId, EncoderProfileInfo,
        HardwarePreference, VideoCodec,
    };

    fn openh264(installed: bool, hardware_encoder: bool) -> CodecDownloadInfo {
        CodecDownloadInfo {
            download_id: "openh264".into(),
            name: "Cisco's OpenH264 (software H.264)".into(),
            codec: VideoCodec::H264,
            version: "2.6.0".into(),
            notice: "OpenH264 Video Codec provided by Cisco Systems, Inc.".into(),
            license_url: "https://www.openh264.org/BINARY_LICENSE.txt".into(),
            license_text: "the licence".into(),
            installed,
            hardware_encoder,
        }
    }

    fn profile(name: &str, codec: VideoCodec, hardware: HardwarePreference) -> EncoderProfileInfo {
        let mut profile = EncoderProfile::DEFAULT;
        profile.video.codec = codec;
        profile.video.hardware = hardware;
        EncoderProfileInfo {
            profile_id: EncoderProfileId::new(format!("enc_{name}")),
            name: name.into(),
            profile,
        }
    }

    #[test]
    fn offered_on_a_machine_without_hardware() {
        let reason = offer_reason(&openh264(false, false), &[]).expect("offered");
        assert!(reason.contains("no hardware H.264 encoder"), "{reason}");
        assert!(reason.contains("Cisco's OpenH264"), "{reason}");
    }

    #[test]
    fn offered_for_a_software_profile_only_when_there_is_hardware() {
        let download = openh264(false, true);
        assert_eq!(offer_reason(&download, &[]), None);
        let hardware = [profile(
            "Stream",
            VideoCodec::H264,
            HardwarePreference::Prefer,
        )];
        assert_eq!(offer_reason(&download, &hardware), None);
        let hevc = [profile(
            "Archive",
            VideoCodec::Hevc,
            HardwarePreference::Avoid,
        )];
        assert_eq!(offer_reason(&download, &hevc), None);
        let software = [
            profile("Stream", VideoCodec::H264, HardwarePreference::Avoid),
            profile("Backup", VideoCodec::H264, HardwarePreference::Avoid),
        ];
        let reason = offer_reason(&download, &software).expect("offered");
        assert!(
            reason.starts_with("Stream, Backup ask for software H.264"),
            "{reason}"
        );
    }

    #[test]
    fn never_offered_once_installed() {
        let software = [profile(
            "Stream",
            VideoCodec::H264,
            HardwarePreference::Avoid,
        )];
        assert_eq!(offer_reason(&openh264(true, false), &software), None);
        assert!(!needed_by_draft(
            &openh264(true, false),
            VideoCodec::H264,
            HardwarePreference::Avoid
        ));
    }

    #[test]
    fn a_draft_needs_it_for_software_h264() {
        let with_hardware = openh264(false, true);
        let without = openh264(false, false);
        assert!(needed_by_draft(
            &with_hardware,
            VideoCodec::H264,
            HardwarePreference::Avoid
        ));
        assert!(!needed_by_draft(
            &with_hardware,
            VideoCodec::H264,
            HardwarePreference::Prefer
        ));
        assert!(needed_by_draft(
            &without,
            VideoCodec::H264,
            HardwarePreference::Prefer
        ));
        assert!(!needed_by_draft(
            &without,
            VideoCodec::H264,
            HardwarePreference::Require
        ));
        assert!(!needed_by_draft(
            &without,
            VideoCodec::Hevc,
            HardwarePreference::Avoid
        ));
    }

    #[test]
    fn the_button_names_cisco_and_the_release() {
        assert_eq!(
            download_label(&openh264(false, false)),
            "Download Cisco's OpenH264 2.6.0"
        );
    }
}
