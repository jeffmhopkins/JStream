//! The YouTube Live API client: [`LiveClient`] lists and creates broadcasts and
//! streams, binds them, transitions broadcasts, and reads stream health.
//!
//! Resources mirror the API's JSON (`snippet`, `status`, `contentDetails`, `cdn`), with
//! the fields JStream reads. Unknown fields are ignored, and enum values the API adds
//! later parse as `Other`, so a new value doesn't break a list. Times stay RFC 3339
//! strings, as the API sends them.
//!
//! A stream's key (`cdn.ingestionInfo.streamName`) is parsed straight into a
//! [`Secret`] and never prints (R-SEC-001, docs/RULES.md#R-SEC-001).

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use jstream_platform::Secret;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::remote_text;
use crate::http::{Body, HttpRequest, HttpResponse, HttpTransport, Method, encode_pairs};
use crate::{AccessToken, OAuthClient, Scope, YoutubeError};

/// The Live API's base URL.
pub const API_BASE: &str = "https://www.googleapis.com/youtube/v3";

/// The most results one list call returns (the API's limit).
pub const MAX_PAGE_SIZE: u8 = 50;

/// The `part`s requested for broadcasts: every one the methods here accept.
const BROADCAST_PARTS: &str = "id,snippet,contentDetails,status";

/// The `part`s `liveStreams.list` accepts.
const STREAM_LIST_PARTS: &str = "id,snippet,cdn,status";

/// The `part`s `liveStreams.insert` writes and returns.
const STREAM_INSERT_PARTS: &str = "id,snippet,cdn,contentDetails,status";

/// A signed-in Live API client.
///
/// It holds the refresh token and the current access token, refreshes the access token
/// before it expires, and retries a call once with a fresh token if the API answers 401.
/// Calls block, and take `&mut self` because they may refresh.
///
/// When Google rotates the refresh token,
/// [`take_rotated_refresh_token`](Self::take_rotated_refresh_token) hands the new one
/// over, for the host to store in place of the old one.
pub struct LiveClient {
    transport: Arc<dyn HttpTransport>,
    oauth: OAuthClient,
    refresh_token: Secret,
    rotated: bool,
    access: Option<AccessToken>,
}

impl fmt::Debug for LiveClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveClient")
            .field("oauth", &self.oauth)
            .field("access", &self.access)
            .finish_non_exhaustive()
    }
}

impl LiveClient {
    /// A client for the grant `refresh_token` belongs to. The first call refreshes.
    pub fn new(
        transport: Arc<dyn HttpTransport>,
        oauth: OAuthClient,
        refresh_token: Secret,
    ) -> Self {
        Self {
            transport,
            oauth,
            refresh_token,
            rotated: false,
            access: None,
        }
    }

    /// A client starting from a finished sign-in, so the first call needs no refresh.
    pub fn from_grant(
        transport: Arc<dyn HttpTransport>,
        oauth: OAuthClient,
        grant: crate::Grant,
    ) -> Self {
        let mut client = Self::new(transport, oauth, grant.refresh);
        client.access = Some(grant.access);
        client
    }

    /// The scopes of the current access token, or `None` before the first token.
    pub fn granted_scopes(&self) -> Option<&[Scope]> {
        self.access.as_ref().map(AccessToken::scopes)
    }

    /// The new refresh token, once, if Google rotated it since the last call.
    pub fn take_rotated_refresh_token(&mut self) -> Option<Secret> {
        std::mem::take(&mut self.rotated).then(|| Secret::new(self.refresh_token.expose()))
    }

    /// Broadcasts matching `filter` (`liveBroadcasts.list`). Needs [`Scope::Readonly`].
    ///
    /// # Errors
    /// [`YoutubeError::InvalidArgument`] for an empty or too long ID list or page size;
    /// API, OAuth and transport errors.
    pub fn list_broadcasts(
        &mut self,
        filter: &BroadcastFilter,
        page: &PageRequest,
    ) -> Result<Page<Broadcast>, YoutubeError> {
        self.list_broadcasts_of_type(filter, None, page)
    }

    /// [`list_broadcasts`](Self::list_broadcasts) with a `broadcastType` filter. The API
    /// leaves persistent ("Stream now") broadcasts out unless asked for `all` or
    /// `persistent`. `None` sends no filter, which the API takes as `event`.
    ///
    /// # Errors
    /// [`YoutubeError::InvalidArgument`] for a type with an ID list (the API takes it
    /// only with `mine` or `broadcastStatus`); as
    /// [`list_broadcasts`](Self::list_broadcasts).
    pub fn list_broadcasts_of_type(
        &mut self,
        filter: &BroadcastFilter,
        broadcast_type: Option<BroadcastType>,
        page: &PageRequest,
    ) -> Result<Page<Broadcast>, YoutubeError> {
        let mut query = vec![("part", BROADCAST_PARTS.to_owned())];
        match filter {
            BroadcastFilter::Mine => query.push(("mine", "true".into())),
            BroadcastFilter::Status(status) => {
                query.push(("broadcastStatus", status.as_str().into()));
            }
            BroadcastFilter::Ids(_) if broadcast_type.is_some() => {
                return Err(YoutubeError::InvalidArgument {
                    reason: "a broadcast type can't be combined with an ID list",
                });
            }
            BroadcastFilter::Ids(ids) => query.push(("id", join_ids(ids)?)),
        }
        if let Some(broadcast_type) = broadcast_type {
            query.push(("broadcastType", broadcast_type.as_str().into()));
        }
        page.push_to(&mut query)?;
        self.call(
            Method::Get,
            "liveBroadcasts",
            &query,
            Body::Empty,
            "liveBroadcasts.list",
            Scope::Readonly,
        )
    }

    /// One broadcast by ID.
    ///
    /// # Errors
    /// [`YoutubeError::NotFound`] when there is none with that ID; as
    /// [`list_broadcasts`](Self::list_broadcasts).
    pub fn broadcast(&mut self, id: &str) -> Result<Broadcast, YoutubeError> {
        let page = self.list_broadcasts(
            &BroadcastFilter::Ids(vec![id.to_owned()]),
            &PageRequest::default(),
        )?;
        first(page, "broadcast", id)
    }

    /// Creates a broadcast (`liveBroadcasts.insert`). Needs [`Scope::Manage`].
    ///
    /// # Errors
    /// [`YoutubeError::InvalidArgument`] for an empty title or start time, or
    /// [`Privacy::Other`]; API, OAuth and transport errors.
    pub fn insert_broadcast(&mut self, new: &NewBroadcast) -> Result<Broadcast, YoutubeError> {
        let body = new.to_json()?;
        let query = [("part", BROADCAST_PARTS.to_owned())];
        self.call(
            Method::Post,
            "liveBroadcasts",
            &query,
            Body::Json(body),
            "liveBroadcasts.insert",
            Scope::Manage,
        )
    }

    /// Binds `stream_id` to the broadcast, or unbinds its stream when `None`
    /// (`liveBroadcasts.bind`). Needs [`Scope::Manage`].
    ///
    /// # Errors
    /// API (such as `liveBroadcastNotFound`), OAuth and transport errors.
    pub fn bind(
        &mut self,
        broadcast_id: &str,
        stream_id: Option<&str>,
    ) -> Result<Broadcast, YoutubeError> {
        let mut query = vec![
            ("id", non_empty(broadcast_id)?),
            ("part", BROADCAST_PARTS.to_owned()),
        ];
        if let Some(stream_id) = stream_id {
            query.push(("streamId", non_empty(stream_id)?));
        }
        self.call(
            Method::Post,
            "liveBroadcasts/bind",
            &query,
            Body::Empty,
            "liveBroadcasts.bind",
            Scope::Manage,
        )
    }

    /// Moves the broadcast to `to` (`liveBroadcasts.transition`). Needs
    /// [`Scope::Manage`]. `Testing` and `Live` need the bound stream to be `active`.
    ///
    /// # Errors
    /// API errors such as `redundantTransition`, `invalidTransition` and
    /// `errorStreamInactive` (each a 403 with that reason); OAuth and transport errors.
    pub fn transition(
        &mut self,
        broadcast_id: &str,
        to: Transition,
    ) -> Result<Broadcast, YoutubeError> {
        let query = [
            ("broadcastStatus", to.as_str().to_owned()),
            ("id", non_empty(broadcast_id)?),
            ("part", BROADCAST_PARTS.to_owned()),
        ];
        self.call(
            Method::Post,
            "liveBroadcasts/transition",
            &query,
            Body::Empty,
            "liveBroadcasts.transition",
            Scope::Manage,
        )
    }

    /// Streams matching `filter` (`liveStreams.list`). Needs [`Scope::Readonly`].
    ///
    /// # Errors
    /// As [`list_broadcasts`](Self::list_broadcasts).
    pub fn list_streams(
        &mut self,
        filter: &StreamFilter,
        page: &PageRequest,
    ) -> Result<Page<LiveStream>, YoutubeError> {
        let mut query = vec![("part", STREAM_LIST_PARTS.to_owned())];
        match filter {
            StreamFilter::Mine => query.push(("mine", "true".into())),
            StreamFilter::Ids(ids) => query.push(("id", join_ids(ids)?)),
        }
        page.push_to(&mut query)?;
        self.call(
            Method::Get,
            "liveStreams",
            &query,
            Body::Empty,
            "liveStreams.list",
            Scope::Readonly,
        )
    }

    /// One stream by ID, with its ingestion details and key.
    ///
    /// # Errors
    /// [`YoutubeError::NotFound`] when there is none with that ID; as
    /// [`list_streams`](Self::list_streams).
    pub fn stream(&mut self, id: &str) -> Result<LiveStream, YoutubeError> {
        let page = self.list_streams(
            &StreamFilter::Ids(vec![id.to_owned()]),
            &PageRequest::default(),
        )?;
        first(page, "stream", id)
    }

    /// Creates a stream (`liveStreams.insert`). Needs [`Scope::Manage`]. The response
    /// carries the new stream's key.
    ///
    /// # Errors
    /// [`YoutubeError::InvalidArgument`] for a bad title or a resolution and frame rate
    /// that aren't both `Variable` or both fixed; API, OAuth and transport errors.
    pub fn insert_stream(&mut self, new: &NewStream) -> Result<LiveStream, YoutubeError> {
        let body = new.json_body()?;
        let query = [("part", STREAM_INSERT_PARTS.to_owned())];
        self.call(
            Method::Post,
            "liveStreams",
            &query,
            Body::Json(body),
            "liveStreams.insert",
            Scope::Manage,
        )
    }

    /// The stream's status and health (`liveStreams.list` with `part=id,status`),
    /// for the engine to poll while live. Needs [`Scope::Readonly`].
    ///
    /// # Errors
    /// [`YoutubeError::NotFound`] when there is no stream with that ID; API, OAuth and
    /// transport errors.
    pub fn stream_health(&mut self, stream_id: &str) -> Result<LiveStreamStatus, YoutubeError> {
        let query = [
            ("part", "id,status".to_owned()),
            ("id", non_empty(stream_id)?),
        ];
        let page: Page<HealthOnly> = self.call(
            Method::Get,
            "liveStreams",
            &query,
            Body::Empty,
            "liveStreams.list",
            Scope::Readonly,
        )?;
        first(page, "stream", stream_id).map(|s| s.status)
    }

    /// Sends one API call with a fresh token, refreshing and retrying once on a 401.
    pub(crate) fn call<T: DeserializeOwned>(
        &mut self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Body,
        endpoint: &'static str,
        needed: Scope,
    ) -> Result<T, YoutubeError> {
        let url = format!(
            "{API_BASE}/{path}?{}",
            encode_pairs(query.iter().map(|(n, v)| (*n, v.as_str())))
        );
        let mut refreshed = false;
        if self
            .access
            .as_ref()
            .is_none_or(|t| t.is_stale(Instant::now()))
        {
            self.refresh()?;
            refreshed = true;
        }
        // A token without a scope list (one the host restored) is let through, and the
        // API decides.
        if let Some(token) = &self.access
            && !token.scopes().is_empty()
            && !token.allows(needed)
        {
            return Err(YoutubeError::MissingScope { needed });
        }
        let body_bytes = body.bytes().to_vec();
        let mut body = Some(body);
        loop {
            let token = self.access.as_ref().map(AccessToken::secret);
            let request = HttpRequest {
                method,
                url: url.clone(),
                bearer: token,
                body: body.take().unwrap_or_else(|| rebuild(&body_bytes)),
                endpoint,
            };
            let response = self.transport.send(request)?;
            if response.status == 401 {
                if refreshed {
                    return Err(YoutubeError::SignInRequired);
                }
                self.refresh()?;
                refreshed = true;
                continue;
            }
            return parse(endpoint, &response);
        }
    }

    fn refresh(&mut self) -> Result<(), YoutubeError> {
        let refreshed = self.oauth.refresh(&*self.transport, &self.refresh_token)?;
        if let Some(rotated) = refreshed.rotated_refresh_token {
            self.refresh_token = rotated;
            self.rotated = true;
        }
        self.access = Some(refreshed.access);
        Ok(())
    }
}

/// The body for a retry. Live API bodies are JSON or empty, never a form.
fn rebuild(bytes: &[u8]) -> Body {
    if bytes.is_empty() {
        Body::Empty
    } else {
        Body::Json(bytes.to_vec())
    }
}

/// A successful response's resource, or the API error it carries.
fn parse<T: DeserializeOwned>(
    endpoint: &'static str,
    response: &HttpResponse,
) -> Result<T, YoutubeError> {
    if response.is_success() {
        return serde_json::from_slice(response.body())
            .map_err(|e| YoutubeError::json(endpoint, &e));
    }
    #[derive(Deserialize)]
    struct Envelope {
        error: ApiError,
    }
    #[derive(Deserialize)]
    struct ApiError {
        #[serde(default)]
        message: String,
        #[serde(default)]
        errors: Vec<ApiErrorItem>,
    }
    #[derive(Deserialize)]
    struct ApiErrorItem {
        #[serde(default)]
        reason: Option<String>,
    }
    let (reason, message) = match serde_json::from_slice::<Envelope>(response.body()) {
        Ok(env) => (
            env.error
                .errors
                .into_iter()
                .find_map(|e| e.reason)
                .map(|r| remote_text(&r)),
            remote_text(&env.error.message),
        ),
        Err(_) => (
            None,
            "the error response is not YouTube's JSON error".to_owned(),
        ),
    };
    Err(YoutubeError::Api {
        status: response.status,
        reason,
        message,
    })
}

pub(crate) fn first<T>(page: Page<T>, kind: &'static str, id: &str) -> Result<T, YoutubeError> {
    page.items
        .into_iter()
        .next()
        .ok_or_else(|| YoutubeError::NotFound {
            kind,
            id: id.to_owned(),
        })
}

pub(crate) fn non_empty(id: &str) -> Result<String, YoutubeError> {
    if id.is_empty() {
        return Err(YoutubeError::InvalidArgument {
            reason: "an ID is empty",
        });
    }
    Ok(id.to_owned())
}

/// The comma-separated `id` filter: 1 to 50 IDs, none empty or holding a comma.
fn join_ids(ids: &[String]) -> Result<String, YoutubeError> {
    let bad = |reason| Err(YoutubeError::InvalidArgument { reason });
    if ids.is_empty() {
        return bad("the ID list is empty");
    }
    if ids.len() > usize::from(MAX_PAGE_SIZE) {
        return bad("the ID list is longer than 50");
    }
    if ids.iter().any(|id| id.is_empty() || id.contains(',')) {
        return bad("an ID is empty or holds a comma");
    }
    Ok(ids.join(","))
}

/// Which broadcasts [`LiveClient::list_broadcasts`] returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BroadcastFilter {
    /// The signed-in channel's broadcasts (`mine=true`).
    Mine,
    /// The channel's broadcasts in one state (`broadcastStatus`).
    Status(BroadcastStatusFilter),
    /// These broadcasts (`id`), at most 50.
    Ids(Vec<String>),
}

/// The `broadcastStatus` filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BroadcastStatusFilter {
    /// Live now.
    Active,
    /// Every broadcast.
    All,
    /// Finished.
    Completed,
    /// Not started yet.
    Upcoming,
}

impl BroadcastStatusFilter {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::All => "all",
            Self::Completed => "completed",
            Self::Upcoming => "upcoming",
        }
    }
}

/// The `broadcastType` filter: scheduled events, persistent broadcasts, or both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BroadcastType {
    /// Both kinds.
    All,
    /// Scheduled events, the API's default.
    Event,
    /// The channel's persistent ("Stream now") broadcast.
    Persistent,
}

impl BroadcastType {
    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Event => "event",
            Self::Persistent => "persistent",
        }
    }
}

/// Which streams [`LiveClient::list_streams`] returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamFilter {
    /// The signed-in channel's streams (`mine=true`).
    Mine,
    /// These streams (`id`), at most 50.
    Ids(Vec<String>),
}

/// Which page of a list to return.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRequest {
    /// Results per page, 0 to [`MAX_PAGE_SIZE`]. Defaults to 50.
    pub max_results: u8,
    /// A `next_page_token` or `prev_page_token` from an earlier page.
    pub page_token: Option<String>,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            max_results: MAX_PAGE_SIZE,
            page_token: None,
        }
    }
}

impl PageRequest {
    fn push_to(&self, query: &mut Vec<(&str, String)>) -> Result<(), YoutubeError> {
        if self.max_results > MAX_PAGE_SIZE {
            return Err(YoutubeError::InvalidArgument {
                reason: "max_results is above 50",
            });
        }
        query.push(("maxResults", self.max_results.to_string()));
        if let Some(token) = &self.page_token {
            query.push(("pageToken", token.clone()));
        }
        Ok(())
    }
}

/// One page of a list.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    /// The resources on this page.
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    /// The token for the next page, if there is one.
    #[serde(default)]
    pub next_page_token: Option<String>,
    /// The token for the previous page, if there is one.
    #[serde(default)]
    pub prev_page_token: Option<String>,
    /// The totals the API reports.
    #[serde(default)]
    pub page_info: Option<PageInfo>,
}

/// A list's `pageInfo`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    /// How many resources match in all.
    #[serde(default)]
    pub total_results: u32,
    /// How many this page holds.
    #[serde(default)]
    pub results_per_page: u32,
}

/// A `liveBroadcast` resource.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Broadcast {
    /// The broadcast's ID, which is also its video ID.
    pub id: String,
    /// Title, description, schedule and the live chat.
    #[serde(default)]
    pub snippet: BroadcastSnippet,
    /// Life-cycle, privacy and recording state.
    #[serde(default)]
    pub status: BroadcastStatus,
    /// The bound stream and the broadcast's settings.
    #[serde(default)]
    pub content_details: BroadcastContentDetails,
}

/// A broadcast's `snippet`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BroadcastSnippet {
    /// The title.
    #[serde(default)]
    pub title: String,
    /// The description.
    #[serde(default)]
    pub description: String,
    /// The owning channel.
    #[serde(default)]
    pub channel_id: Option<String>,
    /// When it is scheduled to start (RFC 3339).
    #[serde(default)]
    pub scheduled_start_time: Option<String>,
    /// When it is scheduled to end (RFC 3339).
    #[serde(default)]
    pub scheduled_end_time: Option<String>,
    /// When it went live, once it has.
    #[serde(default)]
    pub actual_start_time: Option<String>,
    /// When it ended, once it has.
    #[serde(default)]
    pub actual_end_time: Option<String>,
    /// Its live chat's ID, for reading and sending chat later.
    #[serde(default)]
    pub live_chat_id: Option<String>,
}

/// A broadcast's `status`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BroadcastStatus {
    /// Where it is in its life cycle.
    #[serde(default)]
    pub life_cycle_status: Option<LifeCycleStatus>,
    /// Who can watch it.
    #[serde(default)]
    pub privacy_status: Option<Privacy>,
    /// `notRecording`, `recording` or `recorded`.
    #[serde(default)]
    pub recording_status: Option<String>,
    /// Whether the channel declared it made for kids.
    #[serde(default)]
    pub self_declared_made_for_kids: Option<bool>,
}

/// `status.lifeCycleStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LifeCycleStatus {
    /// Created, not yet ready.
    Created,
    /// Ready to go to testing or live.
    Ready,
    /// Moving to testing.
    TestStarting,
    /// Visible only to the channel, through the monitor stream.
    Testing,
    /// Moving to live.
    LiveStarting,
    /// Live.
    Live,
    /// Finished.
    Complete,
    /// Removed by YouTube.
    Revoked,
    /// A value this crate doesn't know.
    #[serde(other)]
    Other,
}

/// Who can watch a broadcast.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Privacy {
    /// Only the channel.
    Private,
    /// Anyone.
    Public,
    /// Anyone with the link.
    Unlisted,
    /// A value this crate doesn't know. Refused when creating a broadcast.
    #[serde(other)]
    Other,
}

/// A broadcast's `contentDetails`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BroadcastContentDetails {
    /// The bound stream, if any.
    #[serde(default)]
    pub bound_stream_id: Option<String>,
    /// Whether YouTube starts the broadcast when the stream becomes active.
    #[serde(default)]
    pub enable_auto_start: Option<bool>,
    /// Whether YouTube ends the broadcast when the stream stops.
    #[serde(default)]
    pub enable_auto_stop: Option<bool>,
    /// Whether viewers can rewind.
    #[serde(default)]
    pub enable_dvr: Option<bool>,
    /// Whether it can be embedded.
    #[serde(default)]
    pub enable_embed: Option<bool>,
    /// Whether it is archived from the start.
    #[serde(default)]
    pub record_from_start: Option<bool>,
    /// The monitor stream, which `testing` needs.
    #[serde(default)]
    pub monitor_stream: Option<MonitorStream>,
}

/// `contentDetails.monitorStream`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorStream {
    /// Whether the monitor stream is on.
    #[serde(default)]
    pub enable_monitor_stream: Option<bool>,
    /// The broadcast delay, in milliseconds.
    #[serde(default)]
    pub broadcast_stream_delay_ms: Option<u32>,
}

/// A broadcast to create. [`NewBroadcast::new`] fills in JStream's defaults: no
/// automatic start or stop (JStream transitions the broadcast itself), the monitor
/// stream on (so `testing` works), and not made for kids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewBroadcast {
    /// The title. Required.
    pub title: String,
    /// The description.
    pub description: String,
    /// When it is scheduled to start, RFC 3339. Required by the API.
    pub scheduled_start_time: String,
    /// When it is scheduled to end, RFC 3339.
    pub scheduled_end_time: Option<String>,
    /// Who can watch it.
    pub privacy: Privacy,
    /// Whether it is made for kids (`selfDeclaredMadeForKids`).
    pub made_for_kids: bool,
    /// `enableAutoStart`.
    pub enable_auto_start: bool,
    /// `enableAutoStop`.
    pub enable_auto_stop: bool,
    /// `monitorStream.enableMonitorStream`.
    pub enable_monitor_stream: bool,
}

impl NewBroadcast {
    /// A broadcast titled `title`, scheduled for `scheduled_start_time`, with JStream's
    /// defaults.
    pub fn new(
        title: impl Into<String>,
        scheduled_start_time: impl Into<String>,
        privacy: Privacy,
    ) -> Self {
        Self {
            title: title.into(),
            description: String::new(),
            scheduled_start_time: scheduled_start_time.into(),
            scheduled_end_time: None,
            privacy,
            made_for_kids: false,
            enable_auto_start: false,
            enable_auto_stop: false,
            enable_monitor_stream: true,
        }
    }

    fn to_json(&self) -> Result<Vec<u8>, YoutubeError> {
        let bad = |reason| Err(YoutubeError::InvalidArgument { reason });
        if self.title.trim().is_empty() {
            return bad("a broadcast needs a title");
        }
        if self.scheduled_start_time.is_empty() {
            return bad("a broadcast needs a scheduled start time");
        }
        if self.privacy == Privacy::Other {
            return bad("a broadcast's privacy must be private, public or unlisted");
        }
        let mut snippet = serde_json::json!({
            "title": self.title,
            "description": self.description,
            "scheduledStartTime": self.scheduled_start_time,
        });
        if let Some(end) = &self.scheduled_end_time {
            snippet["scheduledEndTime"] = end.as_str().into();
        }
        let body = serde_json::json!({
            "snippet": snippet,
            "status": {
                "privacyStatus": self.privacy,
                "selfDeclaredMadeForKids": self.made_for_kids,
            },
            "contentDetails": {
                "enableAutoStart": self.enable_auto_start,
                "enableAutoStop": self.enable_auto_stop,
                "monitorStream": { "enableMonitorStream": self.enable_monitor_stream },
            },
        });
        Ok(body.to_string().into_bytes())
    }
}

/// The states [`LiveClient::transition`] moves a broadcast to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    /// Visible only through the monitor stream.
    Testing,
    /// Visible to viewers.
    Live,
    /// Ended.
    Complete,
}

impl Transition {
    fn as_str(self) -> &'static str {
        match self {
            Self::Testing => "testing",
            Self::Live => "live",
            Self::Complete => "complete",
        }
    }
}

/// A `liveStream` resource: the ingest point a broadcast is bound to.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveStream {
    /// The stream's ID.
    pub id: String,
    /// Title and description.
    #[serde(default)]
    pub snippet: StreamSnippet,
    /// Ingestion settings, addresses and key.
    #[serde(default)]
    pub cdn: Cdn,
    /// Status and health.
    #[serde(default)]
    pub status: LiveStreamStatus,
    /// Whether the stream is reusable (returned by `insert` only).
    #[serde(default)]
    pub content_details: StreamContentDetails,
}

/// A stream's `snippet`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamSnippet {
    /// The title.
    #[serde(default)]
    pub title: String,
    /// The description.
    #[serde(default)]
    pub description: String,
    /// The owning channel.
    #[serde(default)]
    pub channel_id: Option<String>,
}

/// A stream's `cdn`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cdn {
    /// `rtmp` (which includes RTMPS), `dash` or `hls`.
    #[serde(default)]
    pub ingestion_type: Option<String>,
    /// Such as `1080p` or `variable`.
    #[serde(default)]
    pub resolution: Option<String>,
    /// `30fps`, `60fps` or `variable`.
    #[serde(default)]
    pub frame_rate: Option<String>,
    /// Where to send it, and the key.
    #[serde(default)]
    pub ingestion_info: IngestionInfo,
}

/// `cdn.ingestionInfo`: the ingest addresses and the stream key.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestionInfo {
    /// The stream key. It never prints.
    #[serde(default, deserialize_with = "secret")]
    pub stream_name: Option<Secret>,
    /// The primary RTMP address, such as `rtmp://a.rtmp.youtube.com/live2`.
    #[serde(default)]
    pub ingestion_address: Option<String>,
    /// The backup RTMP address.
    #[serde(default)]
    pub backup_ingestion_address: Option<String>,
    /// The primary RTMPS address, which JStream's YouTube preset uses.
    #[serde(default)]
    pub rtmps_ingestion_address: Option<String>,
    /// The backup RTMPS address.
    #[serde(default)]
    pub rtmps_backup_ingestion_address: Option<String>,
}

/// A stream's `contentDetails`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamContentDetails {
    /// Whether it can be bound to more than one broadcast.
    #[serde(default)]
    pub is_reusable: Option<bool>,
}

/// A stream's `status`: whether YouTube is receiving it, and how well.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveStreamStatus {
    /// Whether data is arriving.
    #[serde(default)]
    pub stream_status: Option<StreamStatus>,
    /// YouTube's view of the stream's health.
    #[serde(default)]
    pub health_status: Option<StreamHealth>,
}

/// `status.streamStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamStatus {
    /// YouTube is receiving data.
    Active,
    /// Created, settings not yet valid.
    Created,
    /// An error.
    Error,
    /// Not receiving data.
    Inactive,
    /// Settings valid, no data yet.
    Ready,
    /// A value this crate doesn't know.
    #[serde(other)]
    Other,
}

/// `status.healthStatus`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamHealth {
    /// The overall health.
    #[serde(default)]
    pub status: Option<HealthStatus>,
    /// When YouTube last updated it, in Unix seconds.
    #[serde(default, deserialize_with = "u64_or_string")]
    pub last_update_time_seconds: Option<u64>,
    /// What YouTube finds wrong with the stream's settings.
    #[serde(default)]
    pub configuration_issues: Vec<ConfigurationIssue>,
}

/// `healthStatus.status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HealthStatus {
    /// No issues.
    Good,
    /// Minor issues.
    Ok,
    /// Serious issues.
    Bad,
    /// No data to judge by.
    NoData,
    /// A value this crate doesn't know.
    #[serde(other)]
    Other,
}

/// One entry of `healthStatus.configurationIssues`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurationIssue {
    /// The issue type, such as `bitrateLow` or `gopSizeLong`. The API lists about
    /// thirty and adds more, so it stays a string.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// How serious it is.
    #[serde(default)]
    pub severity: Option<Severity>,
    /// A short explanation.
    #[serde(default)]
    pub reason: String,
    /// The details.
    #[serde(default)]
    pub description: String,
}

/// A configuration issue's severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// Informational.
    Info,
    /// Might be a problem.
    Warning,
    /// Has to be fixed.
    Error,
    /// A value this crate doesn't know.
    #[serde(other)]
    Other,
}

/// A stream to create. [`NewStream::new`] asks for RTMP ingestion with variable
/// resolution and frame rate, so the encoder's settings decide, and a reusable stream,
/// so one key serves every broadcast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewStream<'a> {
    /// The title, 1 to 128 characters.
    pub title: &'a str,
    /// The description, at most 10 000 characters.
    pub description: &'a str,
    /// The resolution.
    pub resolution: Resolution,
    /// The frame rate. `Variable` exactly when the resolution is.
    pub frame_rate: FrameRate,
    /// Whether it can be bound to several broadcasts.
    pub reusable: bool,
}

impl<'a> NewStream<'a> {
    /// A reusable variable-format RTMP stream titled `title`.
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            description: "",
            resolution: Resolution::Variable,
            frame_rate: FrameRate::Variable,
            reusable: true,
        }
    }

    fn json_body(self) -> Result<Vec<u8>, YoutubeError> {
        let bad = |reason| Err(YoutubeError::InvalidArgument { reason });
        let title_len = self.title.chars().count();
        if self.title.trim().is_empty() || title_len > 128 {
            return bad("a stream's title must be 1 to 128 characters");
        }
        if self.description.chars().count() > 10_000 {
            return bad("a stream's description must be at most 10000 characters");
        }
        if (self.resolution == Resolution::Variable) != (self.frame_rate == FrameRate::Variable) {
            return bad("resolution and frame rate must both be variable, or both fixed");
        }
        let body = serde_json::json!({
            "snippet": { "title": self.title, "description": self.description },
            "cdn": {
                "ingestionType": "rtmp",
                "resolution": self.resolution.as_str(),
                "frameRate": self.frame_rate.as_str(),
            },
            "contentDetails": { "isReusable": self.reusable },
        });
        Ok(body.to_string().into_bytes())
    }
}

/// `cdn.resolution` for a new stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// 240p.
    P240,
    /// 360p.
    P360,
    /// 480p.
    P480,
    /// 720p.
    P720,
    /// 1080p.
    P1080,
    /// 1440p.
    P1440,
    /// 2160p.
    P2160,
    /// Whatever the encoder sends.
    Variable,
}

impl Resolution {
    fn as_str(self) -> &'static str {
        match self {
            Self::P240 => "240p",
            Self::P360 => "360p",
            Self::P480 => "480p",
            Self::P720 => "720p",
            Self::P1080 => "1080p",
            Self::P1440 => "1440p",
            Self::P2160 => "2160p",
            Self::Variable => "variable",
        }
    }
}

/// `cdn.frameRate` for a new stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameRate {
    /// 30 fps.
    Fps30,
    /// 60 fps.
    Fps60,
    /// Whatever the encoder sends.
    Variable,
}

impl FrameRate {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fps30 => "30fps",
            Self::Fps60 => "60fps",
            Self::Variable => "variable",
        }
    }
}

/// A stream read with `part=id,status`.
#[derive(Deserialize)]
struct HealthOnly {
    #[serde(default)]
    status: LiveStreamStatus,
}

/// Moves a JSON string straight into a [`Secret`].
fn secret<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Secret>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.map(Secret::new))
}

/// A uint64, which Google's JSON sends as a string, accepting a number too.
pub(crate) fn u64_or_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Number(u64),
        Text(String),
    }
    match Option::<Raw>::deserialize(d)? {
        None => Ok(None),
        Some(Raw::Number(n)) => Ok(Some(n)),
        Some(Raw::Text(s)) => s
            .parse()
            .map(Some)
            .map_err(|_| serde::de::Error::custom("not an unsigned integer")),
    }
}

#[cfg(test)]
mod tests;
