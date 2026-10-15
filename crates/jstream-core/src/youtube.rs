//! YouTube in the engine (ADR-0041): the sign-in, the account's [`LiveClient`], the
//! broadcast actions and queries, the ingest a `youtube` output publishes to, and the
//! health poll while one runs.
//!
//! **Secrets (R-SEC-001).** The refresh token goes from the code exchange straight to
//! the host's [`OAuthTokenStore`], and the stream key from the Live API straight to the
//! RTMPS connection. Neither is returned, emitted or logged: protocol errors carry
//! `jstream-youtube`'s errors, which never hold a token.
//!
//! **One account.** JStream signs in to one Google account at a time, so its refresh
//! token sits under the fixed, opaque account ID [`TOKEN_ACCOUNT`] in the store. No
//! email address or other personal detail becomes part of a credential's name.
//!
//! **Locks.** `auth` is taken before `client` when both are held. Neither is ever taken
//! under the engine's collection lock, and the client's lock is held across its
//! blocking Live API calls, so YouTube calls run one at a time.

use crate::lock;
use crate::runtime::{LivePublish, PublishError, connect_rtmp};
use jstream_platform::{OAuthTokenStore, Secret};
use jstream_protocol::{
    ActionResult, BroadcastInfo, BroadcastLifeCycle, BroadcastListStatus, BroadcastPrivacy,
    BroadcastTransition, BroadcastType, Event, HealthIssue, IssueSeverity, OutputId, ProtocolError,
    QueryResult, StreamHealth, StreamInfo, StreamStatus, YoutubeAuth,
};
use jstream_youtube::live::{
    self, Broadcast, BroadcastFilter, BroadcastStatusFilter, LiveStream, LiveStreamStatus,
    NewBroadcast, NewStream, PageRequest, Privacy, StreamFilter, Transition,
};
use jstream_youtube::{
    HttpTransport, LiveClient, LoopbackRedirect, MAX_REMOTE_TEXT, OAuthClient, Scope, YoutubeError,
};
use std::fmt;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// The account ID the refresh token is stored under in the [`OAuthTokenStore`].
pub const TOKEN_ACCOUNT: &str = "youtube";

/// How long a sign-in waits for the browser's redirect before giving up.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How often a running `youtube` output's stream health is read. Each read costs one
/// unit of the day's API quota.
pub const HEALTH_INTERVAL: Duration = Duration::from_secs(15);

/// Connects an RTMP(S) publish target to a server URL with a stream key. The engine
/// uses [`connect_rtmp`] unless a test swapped it.
pub(crate) type ConnectRtmp =
    Arc<dyn Fn(&str, &Secret) -> Result<Box<dyn LivePublish>, PublishError> + Send + Sync>;

/// Emits an event to every subscriber, if the engine is still there.
pub(crate) type Emit = Arc<dyn Fn(Event) + Send + Sync>;

/// What the host gives the engine for YouTube: the app's OAuth client, the HTTPS
/// transport, and where the refresh token is kept.
pub struct YoutubeSetup {
    client_id: String,
    client_secret: Option<Secret>,
    transport: Arc<dyn HttpTransport>,
    tokens: Arc<dyn OAuthTokenStore>,
    sign_in_timeout: Duration,
    health_interval: Duration,
}

impl fmt::Debug for YoutubeSetup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("YoutubeSetup")
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret)
            .field("sign_in_timeout", &self.sign_in_timeout)
            .field("health_interval", &self.health_interval)
            .finish_non_exhaustive()
    }
}

impl YoutubeSetup {
    /// The app's Google OAuth client (`client_id`, and its secret if it has one; an
    /// installed app's can't really be kept secret, but it never prints), the transport
    /// every Google call goes through (`jstream_youtube::HttpsTransport` in the app),
    /// and the store that keeps the refresh token.
    pub fn new(
        client_id: impl Into<String>,
        client_secret: Option<Secret>,
        transport: Arc<dyn HttpTransport>,
        tokens: Arc<dyn OAuthTokenStore>,
    ) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret,
            transport,
            tokens,
            sign_in_timeout: SIGN_IN_TIMEOUT,
            health_interval: HEALTH_INTERVAL,
        }
    }

    /// Waits `timeout` for a sign-in's redirect instead of [`SIGN_IN_TIMEOUT`].
    #[must_use]
    pub fn with_sign_in_timeout(mut self, timeout: Duration) -> Self {
        self.sign_in_timeout = timeout;
        self
    }

    /// Reads a running output's stream health every `interval` instead of
    /// [`HEALTH_INTERVAL`].
    #[must_use]
    pub fn with_health_interval(mut self, interval: Duration) -> Self {
        self.health_interval = interval;
        self
    }

    /// A fresh copy of the OAuth client. `OAuthClient` isn't `Clone`, because its
    /// secret isn't.
    fn oauth(&self) -> OAuthClient {
        OAuthClient::new(
            self.client_id.clone(),
            self.client_secret
                .as_ref()
                .map(|secret| Secret::new(secret.expose())),
        )
    }
}

/// The sign-in state.
struct Auth {
    /// What `youtube.status` reports.
    state: YoutubeAuth,
    /// The sign-in waiting for its redirect, if any.
    pending: Option<PendingSignIn>,
    /// Numbers the sign-ins, so one that a sign-out abandoned can't finish.
    generation: u64,
}

struct PendingSignIn {
    generation: u64,
    url: String,
}

/// A running output's health poll. Dropping it ends the poll.
struct HealthWatch {
    output_id: OutputId,
    _stop: Sender<()>,
}

/// Where a `youtube` output publishes: the ingest its broadcast's bound stream names.
pub(crate) struct Ingest {
    pub(crate) url: String,
    pub(crate) key: Secret,
    pub(crate) stream_id: String,
}

/// The engine's YouTube side.
pub(crate) struct Youtube {
    setup: YoutubeSetup,
    auth: Mutex<Auth>,
    client: Mutex<Option<LiveClient>>,
    emit: Emit,
    connect: Mutex<ConnectRtmp>,
    health: Mutex<Option<HealthWatch>>,
}

impl Youtube {
    /// Starts signed in when the store holds a refresh token from an earlier sign-in.
    pub(crate) fn new(setup: YoutubeSetup, emit: Emit) -> Self {
        let client = match setup.tokens.get(TOKEN_ACCOUNT) {
            Ok(token) => token
                .map(|token| LiveClient::new(Arc::clone(&setup.transport), setup.oauth(), token)),
            Err(err) => {
                tracing::error!(error = %err, "reading the YouTube token from the credential store");
                None
            }
        };
        let state = if client.is_some() {
            YoutubeAuth::SignedIn
        } else {
            YoutubeAuth::SignedOut
        };
        Self {
            setup,
            auth: Mutex::new(Auth {
                state,
                pending: None,
                generation: 0,
            }),
            client: Mutex::new(client),
            emit,
            connect: Mutex::new(Arc::new(|url: &str, key: &Secret| {
                connect_rtmp(url, key.expose())
                    .map(|publish| Box::new(publish) as Box<dyn LivePublish>)
            })),
            health: Mutex::new(None),
        }
    }

    /// Replaces how a `youtube` output connects, for tests.
    #[cfg(test)]
    pub(crate) fn set_connect(&self, connect: ConnectRtmp) {
        *lock(&self.connect) = connect;
    }

    /// What `youtube.status` reports.
    pub(crate) fn status(&self) -> QueryResult {
        QueryResult::YoutubeStatus {
            auth: lock(&self.auth).state,
        }
    }

    /// Whether the engine holds a signed-in client. Waits for a YouTube call in flight.
    pub(crate) fn signed_in(&self) -> bool {
        lock(&self.client).is_some()
    }

    /// `youtube.sign_in`: binds the loopback redirect, builds the consent URL for the
    /// full scope, and finishes the sign-in on its own thread.
    pub(crate) fn sign_in(self: &Arc<Self>) -> Result<ActionResult, ProtocolError> {
        let mut auth = lock(&self.auth);
        if let Some(pending) = &auth.pending {
            return Ok(ActionResult::YoutubeSignIn {
                url: pending.url.clone(),
            });
        }
        let redirect = LoopbackRedirect::bind().map_err(failed)?;
        let pending = self
            .setup
            .oauth()
            .authorize(&redirect.redirect_uri(), &[Scope::Manage])
            .map_err(failed)?;
        let url = pending.url().to_owned();
        auth.generation += 1;
        let generation = auth.generation;
        let this = Arc::clone(self);
        let timeout = self.setup.sign_in_timeout;
        thread::Builder::new()
            .name("jstream-youtube-sign-in".into())
            .spawn(move || {
                let outcome = redirect.wait(&pending, timeout).and_then(|code| {
                    this.setup
                        .oauth()
                        .exchange(&*this.setup.transport, pending, code)
                });
                this.finish_sign_in(generation, outcome);
            })
            .map_err(|err| ProtocolError::ActionFailed {
                reason: format!("starting the YouTube sign-in: {err}"),
            })?;
        auth.pending = Some(PendingSignIn {
            generation,
            url: url.clone(),
        });
        auth.state = YoutubeAuth::SigningIn;
        drop(auth);
        (self.emit)(Event::YoutubeStatusChanged {
            auth: YoutubeAuth::SigningIn,
        });
        tracing::info!("YouTube sign-in started");
        Ok(ActionResult::YoutubeSignIn { url })
    }

    /// Ends sign-in `generation`: keeps the refresh token and the new client, or goes
    /// back to the state before it. Does nothing if a sign-out abandoned it.
    fn finish_sign_in(
        &self,
        generation: u64,
        outcome: Result<jstream_youtube::Grant, YoutubeError>,
    ) {
        let mut auth = lock(&self.auth);
        if auth.pending.as_ref().map(|p| p.generation) != Some(generation) {
            return;
        }
        auth.pending = None;
        let mut client = lock(&self.client);
        match outcome {
            Ok(grant) => match self.setup.tokens.set(TOKEN_ACCOUNT, &grant.refresh) {
                Ok(()) => {
                    *client = Some(LiveClient::from_grant(
                        Arc::clone(&self.setup.transport),
                        self.setup.oauth(),
                        grant,
                    ));
                    tracing::info!("YouTube sign-in finished");
                }
                Err(err) => {
                    tracing::error!(error = %err, "the credential store did not keep the YouTube token");
                }
            },
            Err(err) => tracing::warn!(error = %err, "YouTube sign-in did not finish"),
        }
        auth.state = if client.is_some() {
            YoutubeAuth::SignedIn
        } else {
            YoutubeAuth::SignedOut
        };
        let state = auth.state;
        drop(client);
        drop(auth);
        (self.emit)(Event::YoutubeStatusChanged { auth: state });
    }

    /// `youtube.sign_out`: forgets the client and the stored token, abandons a waiting
    /// sign-in, then revokes the token at Google (best effort).
    pub(crate) fn sign_out(&self) -> Result<(), ProtocolError> {
        let token = {
            let mut auth = lock(&self.auth);
            let token = self
                .setup
                .tokens
                .get(TOKEN_ACCOUNT)
                .and_then(|token| self.setup.tokens.clear(TOKEN_ACCOUNT).map(|()| token))
                .map_err(|err| ProtocolError::ActionFailed {
                    reason: format!("the credential store did not remove the YouTube token: {err}"),
                })?;
            auth.pending = None;
            auth.generation += 1;
            auth.state = YoutubeAuth::SignedOut;
            *lock(&self.client) = None;
            token
        };
        (self.emit)(Event::YoutubeStatusChanged {
            auth: YoutubeAuth::SignedOut,
        });
        tracing::info!("signed out of YouTube");
        if let Some(token) = token
            && let Err(err) = self.setup.oauth().revoke(&*self.setup.transport, &token)
        {
            tracing::warn!(error = %err, "revoking the YouTube token");
        }
        Ok(())
    }

    /// Google no longer accepts the account's token: forget it, as a sign-out would,
    /// without calling Google.
    fn lost_sign_in(&self) {
        let state = {
            let mut auth = lock(&self.auth);
            *lock(&self.client) = None;
            if let Err(err) = self.setup.tokens.clear(TOKEN_ACCOUNT) {
                tracing::error!(error = %err, "removing the refused YouTube token");
            }
            if auth.pending.is_none() {
                auth.state = YoutubeAuth::SignedOut;
            }
            auth.state
        };
        tracing::warn!("YouTube no longer accepts the sign-in");
        (self.emit)(Event::YoutubeStatusChanged { auth: state });
    }

    /// Runs `call` on the signed-in client, then stores a rotated refresh token.
    fn with_client<T>(
        &self,
        call: impl FnOnce(&mut LiveClient) -> Result<T, YoutubeError>,
    ) -> Result<T, ProtocolError> {
        let (outcome, rotated) = {
            let mut client = lock(&self.client);
            let Some(client) = client.as_mut() else {
                return Err(ProtocolError::ActionFailed {
                    reason: "not signed in to YouTube: send `youtube.sign_in`".into(),
                });
            };
            let outcome = call(client);
            (outcome, client.take_rotated_refresh_token())
        };
        if let Some(token) = rotated
            && let Err(err) = self.setup.tokens.set(TOKEN_ACCOUNT, &token)
        {
            tracing::error!(error = %err, "the credential store did not keep the rotated YouTube token");
        }
        match outcome {
            Err(YoutubeError::SignInRequired) => {
                self.lost_sign_in();
                Err(ProtocolError::ActionFailed {
                    reason: "YouTube no longer accepts the sign-in: send `youtube.sign_in`".into(),
                })
            }
            other => other.map_err(failed),
        }
    }

    /// `youtube.list_broadcasts`.
    pub(crate) fn list_broadcasts(
        &self,
        status: BroadcastListStatus,
        broadcast_type: BroadcastType,
        page_token: Option<String>,
    ) -> Result<QueryResult, ProtocolError> {
        let status = match status {
            BroadcastListStatus::Upcoming => BroadcastStatusFilter::Upcoming,
            BroadcastListStatus::Active => BroadcastStatusFilter::Active,
            BroadcastListStatus::Completed => BroadcastStatusFilter::Completed,
            _ => BroadcastStatusFilter::All,
        };
        let broadcast_type = match broadcast_type {
            BroadcastType::Event => live::BroadcastType::Event,
            BroadcastType::Persistent => live::BroadcastType::Persistent,
            _ => live::BroadcastType::All,
        };
        let page = self.with_client(|client| {
            client.list_broadcasts_of_type(
                &BroadcastFilter::Status(status),
                Some(broadcast_type),
                &PageRequest {
                    page_token,
                    ..PageRequest::default()
                },
            )
        })?;
        Ok(QueryResult::YoutubeListBroadcasts {
            broadcasts: page.items.into_iter().map(broadcast_info).collect(),
            next_page_token: page.next_page_token,
        })
    }

    /// `youtube.list_streams`.
    pub(crate) fn list_streams(
        &self,
        page_token: Option<String>,
    ) -> Result<QueryResult, ProtocolError> {
        let page = self.with_client(|client| {
            client.list_streams(
                &StreamFilter::Mine,
                &PageRequest {
                    page_token,
                    ..PageRequest::default()
                },
            )
        })?;
        Ok(QueryResult::YoutubeListStreams {
            streams: page.items.into_iter().map(stream_info).collect(),
            next_page_token: page.next_page_token,
        })
    }

    /// `youtube.create_broadcast`.
    pub(crate) fn create_broadcast(
        &self,
        title: String,
        description: String,
        scheduled_start_time: String,
        privacy: BroadcastPrivacy,
        made_for_kids: bool,
    ) -> Result<ActionResult, ProtocolError> {
        if title.trim().is_empty() {
            return Err(invalid("title", "must not be empty"));
        }
        if scheduled_start_time.trim().is_empty() {
            return Err(invalid("scheduled_start_time", "must not be empty"));
        }
        let privacy = match privacy {
            BroadcastPrivacy::Private => Privacy::Private,
            BroadcastPrivacy::Public => Privacy::Public,
            BroadcastPrivacy::Unlisted => Privacy::Unlisted,
            _ => return Err(invalid("privacy", "must be private, public or unlisted")),
        };
        let mut new = NewBroadcast::new(title, scheduled_start_time, privacy);
        new.description = description;
        new.made_for_kids = made_for_kids;
        let broadcast = self.with_client(|client| client.insert_broadcast(&new))?;
        Ok(ActionResult::YoutubeCreateBroadcast {
            broadcast: broadcast_info(broadcast),
        })
    }

    /// `youtube.bind_broadcast`. Without a stream, creates a reusable one named after
    /// the broadcast first.
    pub(crate) fn bind_broadcast(
        &self,
        broadcast_id: &str,
        stream_id: Option<String>,
    ) -> Result<ActionResult, ProtocolError> {
        let broadcast = self.with_client(|client| {
            let stream_id = match stream_id {
                Some(stream_id) => stream_id,
                None => {
                    let title = client.broadcast(broadcast_id)?.snippet.title;
                    client.insert_stream(&NewStream::new(&title))?.id
                }
            };
            client.bind(broadcast_id, Some(&stream_id))
        })?;
        Ok(ActionResult::YoutubeBindBroadcast {
            broadcast: broadcast_info(broadcast),
        })
    }

    /// `youtube.transition_broadcast`.
    pub(crate) fn transition_broadcast(
        &self,
        broadcast_id: &str,
        to: BroadcastTransition,
    ) -> Result<ActionResult, ProtocolError> {
        let to = match to {
            BroadcastTransition::Testing => Transition::Testing,
            BroadcastTransition::Live => Transition::Live,
            _ => Transition::Complete,
        };
        let broadcast = self.with_client(|client| client.transition(broadcast_id, to))?;
        Ok(ActionResult::YoutubeTransitionBroadcast {
            broadcast: broadcast_info(broadcast),
        })
    }

    /// The `rtmps://` ingest of `broadcast_id`'s bound stream, with its key.
    pub(crate) fn ingest(&self, broadcast_id: &str) -> Result<Ingest, String> {
        let found = self
            .with_client(|client| {
                let broadcast = client.broadcast(broadcast_id)?;
                let Some(stream_id) = broadcast.content_details.bound_stream_id else {
                    return Ok(Err(format!(
                        "YouTube broadcast `{broadcast_id}` has no bound stream: send `youtube.bind_broadcast`"
                    )));
                };
                let stream = client.stream(&stream_id)?;
                Ok(Ok((stream_id, stream)))
            })
            .map_err(|err| err.to_string())?;
        let (stream_id, stream) = found?;
        let info = stream.cdn.ingestion_info;
        let url = info.rtmps_ingestion_address.ok_or_else(|| {
            format!("YouTube gave no RTMPS ingest address for stream `{stream_id}`")
        })?;
        let key = info
            .stream_name
            .ok_or_else(|| format!("YouTube gave no stream key for stream `{stream_id}`"))?;
        Ok(Ingest {
            url,
            key,
            stream_id,
        })
    }

    /// Connects to `ingest`.
    pub(crate) fn connect(&self, ingest: &Ingest) -> Result<Box<dyn LivePublish>, PublishError> {
        let connect = Arc::clone(&lock(&self.connect));
        connect(&ingest.url, &ingest.key)
    }

    /// Starts reading the health of `stream_id` every interval while `alive` says
    /// `output_id` still runs, emitting `youtube.health_changed` when it changes. A poll
    /// already running for another output ends.
    pub(crate) fn watch_health(
        self: &Arc<Self>,
        output_id: OutputId,
        broadcast_id: String,
        stream_id: String,
        alive: Box<dyn Fn() -> bool + Send>,
    ) {
        let (stop, stopped) = mpsc::channel::<()>();
        let this = Arc::downgrade(self);
        let interval = self.setup.health_interval;
        let watched = output_id.clone();
        let spawned = thread::Builder::new()
            .name("jstream-youtube-health".into())
            .spawn(move || {
                let mut last = None;
                loop {
                    match stopped.recv_timeout(interval) {
                        Err(RecvTimeoutError::Timeout) => {}
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                    }
                    if !alive() {
                        return;
                    }
                    let Some(youtube) = this.upgrade() else {
                        return;
                    };
                    match youtube.with_client(|client| client.stream_health(&stream_id)) {
                        Ok(health) => {
                            let event = health_event(&watched, &broadcast_id, &stream_id, health);
                            if last.as_ref() != Some(&event) {
                                last = Some(event.clone());
                                (youtube.emit)(event);
                            }
                        }
                        Err(err) => {
                            tracing::warn!(output_id = %watched, error = %err, "reading YouTube stream health");
                        }
                    }
                }
            });
        match spawned {
            Ok(_) => {
                *lock(&self.health) = Some(HealthWatch {
                    output_id,
                    _stop: stop,
                });
            }
            Err(err) => {
                tracing::error!(output_id = %output_id, error = %err, "starting the YouTube health poll");
            }
        }
    }

    /// Ends the health poll of `output_id`, if one runs. The thread notices at once,
    /// unless a read is in flight, and is not waited for.
    pub(crate) fn unwatch_health(&self, output_id: &OutputId) {
        let mut health = lock(&self.health);
        if health
            .as_ref()
            .is_some_and(|watch| &watch.output_id == output_id)
        {
            *health = None;
        }
    }
}

/// A `jstream-youtube` error as a protocol error. Quota errors say so first
/// (ADR-0041 §5).
fn failed(err: YoutubeError) -> ProtocolError {
    let reason = if err.is_quota() {
        format!("YouTube's API quota is used up: {err}")
    } else {
        format!("YouTube: {err}")
    };
    ProtocolError::ActionFailed { reason }
}

fn invalid(field: &str, reason: &str) -> ProtocolError {
    ProtocolError::InvalidArgument {
        field: field.into(),
        reason: reason.into(),
    }
}

fn broadcast_info(broadcast: Broadcast) -> BroadcastInfo {
    use live::LifeCycleStatus as L;
    let life_cycle = match broadcast.status.life_cycle_status {
        Some(L::Created) => BroadcastLifeCycle::Created,
        Some(L::Ready) => BroadcastLifeCycle::Ready,
        Some(L::TestStarting) => BroadcastLifeCycle::TestStarting,
        Some(L::Testing) => BroadcastLifeCycle::Testing,
        Some(L::LiveStarting) => BroadcastLifeCycle::LiveStarting,
        Some(L::Live) => BroadcastLifeCycle::Live,
        Some(L::Complete) => BroadcastLifeCycle::Complete,
        Some(L::Revoked) => BroadcastLifeCycle::Revoked,
        _ => BroadcastLifeCycle::Other,
    };
    let privacy = match broadcast.status.privacy_status {
        Some(Privacy::Private) => BroadcastPrivacy::Private,
        Some(Privacy::Public) => BroadcastPrivacy::Public,
        Some(Privacy::Unlisted) => BroadcastPrivacy::Unlisted,
        _ => BroadcastPrivacy::Other,
    };
    BroadcastInfo {
        broadcast_id: broadcast.id,
        title: broadcast.snippet.title,
        description: broadcast.snippet.description,
        scheduled_start_time: broadcast.snippet.scheduled_start_time,
        life_cycle,
        privacy,
        bound_stream_id: broadcast.content_details.bound_stream_id,
    }
}

fn stream_info(stream: LiveStream) -> StreamInfo {
    let (status, health, _) = stream_state(stream.status);
    StreamInfo {
        stream_id: stream.id,
        title: stream.snippet.title,
        status,
        health,
    }
}

/// A stream's status, health and issues in the protocol's terms. Issue text is
/// YouTube's, so it is cut and cleaned like its error text.
fn stream_state(status: LiveStreamStatus) -> (StreamStatus, StreamHealth, Vec<HealthIssue>) {
    use live::{HealthStatus as H, Severity as S, StreamStatus as T};
    let stream = match status.stream_status {
        Some(T::Active) => StreamStatus::Active,
        Some(T::Created) => StreamStatus::Created,
        Some(T::Error) => StreamStatus::Error,
        Some(T::Inactive) => StreamStatus::Inactive,
        Some(T::Ready) => StreamStatus::Ready,
        _ => StreamStatus::Other,
    };
    let Some(health) = status.health_status else {
        return (stream, StreamHealth::NoData, Vec::new());
    };
    let rating = match health.status {
        Some(H::Good) => StreamHealth::Good,
        Some(H::Ok) => StreamHealth::Ok,
        Some(H::Bad) => StreamHealth::Bad,
        Some(H::NoData) | None => StreamHealth::NoData,
        _ => StreamHealth::Other,
    };
    let issues = health
        .configuration_issues
        .into_iter()
        .map(|issue| HealthIssue {
            issue: clean(&issue.kind),
            severity: match issue.severity {
                Some(S::Info) => IssueSeverity::Info,
                Some(S::Warning) => IssueSeverity::Warning,
                Some(S::Error) => IssueSeverity::Error,
                _ => IssueSeverity::Other,
            },
            reason: clean(&issue.reason),
            description: clean(&issue.description),
        })
        .collect();
    (stream, rating, issues)
}

fn health_event(
    output_id: &OutputId,
    broadcast_id: &str,
    stream_id: &str,
    status: LiveStreamStatus,
) -> Event {
    let (status, health, issues) = stream_state(status);
    Event::YoutubeHealthChanged {
        output_id: output_id.clone(),
        broadcast_id: broadcast_id.to_owned(),
        stream_id: stream_id.to_owned(),
        status,
        health,
        issues,
    }
}

/// Remote text cut to [`MAX_REMOTE_TEXT`] characters, control characters replaced.
fn clean(text: &str) -> String {
    text.chars()
        .take(MAX_REMOTE_TEXT)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
