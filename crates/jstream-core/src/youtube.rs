//! YouTube in the engine (ADR-0058): the sign-in, the account's [`LiveClient`], the
//! broadcast actions and queries, the ingest a `youtube` output publishes to, the
//! health and audience polls while one runs, and the estimate of the day's API quota
//! (ADR-0041 §3 and §5).
//!
//! **Secrets (R-SEC-001).** The refresh token goes from the code exchange straight to
//! the host's [`OAuthTokenStore`], and the stream key from the Live API straight to the
//! RTMPS connection. Neither is returned, emitted or logged: protocol errors carry
//! `jstream-youtube`'s errors, which never hold a token. A host that redacts its crash
//! reports gives the setup a [`SecretHook`]
//! ([`with_secret_hook`](YoutubeSetup::with_secret_hook)), and each account's client hands
//! it the refresh and access tokens and every stream key it gets, including the key a
//! `youtube` output publishes with (#353).
//!
//! **One account.** JStream signs in to one Google account at a time, so its refresh
//! token sits under the fixed, opaque account ID [`TOKEN_ACCOUNT`] in the store. No
//! email address or other personal detail becomes part of a credential's name.
//!
//! **Quota.** Every Live API call the engine makes is charged to the day's estimate at
//! YouTube's published unit cost before it is sent: 1 for a read (a list, a stream's
//! health, a broadcast's audience) and 50 for a write (an insert, a bind, a
//! transition). The estimate starts again at midnight Pacific Time, when YouTube resets
//! the quota; the engine takes that as UTC−8 all year, so during daylight time its day
//! starts an hour after YouTube's. Audience stats slow to one read every
//! [`STATS_SLOWDOWN`] intervals when less than a fifth of [`DAILY_QUOTA`] is left, and
//! stop when less than a tenth is, so the actions and the health poll keep what is
//! left. A call YouTube refuses for quota marks the day exhausted: stats and health
//! polls stop until the estimate's day ends, and actions still go to YouTube, which
//! answers for itself.
//!
//! **Locks.** `auth` is taken before `client` when both are held. Neither is ever taken
//! under the engine's collection lock, and the client's lock is held across its
//! blocking Live API calls, so YouTube calls run one at a time. `watches` is taken
//! before `stats`. `quota` is a leaf: nothing else is taken while it is held.

use crate::lock;
use crate::runtime::{LivePublish, PublishError, connect_rtmp};
use jstream_platform::{OAuthTokenStore, Secret};
use jstream_protocol::{
    ActionResult, AudienceStatsInfo, BroadcastInfo, BroadcastLifeCycle, BroadcastListStatus,
    BroadcastPrivacy, BroadcastTransition, BroadcastType, Event, HealthIssue, IssueSeverity,
    OutputId, ProtocolError, QueryResult, QuotaInfo, StatsPolling, StreamHealth, StreamInfo,
    StreamStatus, YoutubeAuth,
};
use jstream_youtube::live::{
    self, Broadcast, BroadcastFilter, BroadcastStatusFilter, LiveStream, LiveStreamStatus,
    NewBroadcast, NewStream, PageRequest, Privacy, StreamFilter, Transition,
};
use jstream_youtube::{
    BUILT_IN_CLIENT_ID, BUILT_IN_CLIENT_SECRET, HttpTransport, LiveClient, LoopbackRedirect,
    MAX_REMOTE_TEXT, OAuthClient, Scope, SecretHook, YoutubeError,
};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// The account ID the refresh token is stored under in the [`OAuthTokenStore`].
pub const TOKEN_ACCOUNT: &str = "youtube";

/// How long a sign-in waits for the browser's redirect before giving up.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How often a running `youtube` output's stream health is read. Each read costs one
/// unit of the day's API quota.
pub const HEALTH_INTERVAL: Duration = Duration::from_secs(15);

/// How often a running `youtube` output's audience is read while the day's quota
/// allows ([`jstream_youtube::stats::STATS_INTERVAL`]). Each read costs one unit.
pub const STATS_INTERVAL: Duration = jstream_youtube::stats::STATS_INTERVAL;

/// How many [`STATS_INTERVAL`]s apart audience reads are once the quota runs low.
pub const STATS_SLOWDOWN: u32 = 5;

/// The units of YouTube Data API quota the engine assumes a day has: the default a
/// Google Cloud project gets. A project granted more can say so with
/// [`YoutubeSetup::with_daily_quota`].
pub const DAILY_QUOTA: u64 = 10_000;

/// What a Live API read costs: a list, a stream's health, a broadcast's audience.
const READ_UNITS: u64 = 1;

/// What a Live API write costs: an insert, a bind, a transition.
const WRITE_UNITS: u64 = 50;

/// The offset of the day the quota estimate counts, from UTC: Pacific standard time.
const QUOTA_DAY_OFFSET_SECS: i64 = -8 * 60 * 60;

/// The wall clock the quota's day is read from.
pub(crate) type WallClock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// Connects an RTMP(S) publish target to a server URL with a stream key. The engine
/// uses [`connect_rtmp`] unless a test swapped it.
pub(crate) type ConnectRtmp =
    Arc<dyn Fn(&str, &Secret) -> Result<Box<dyn LivePublish>, PublishError> + Send + Sync>;

/// Emits an event to every subscriber, if the engine is still there.
pub(crate) type Emit = Arc<dyn Fn(Event) + Send + Sync>;

/// Why a YouTube call fails when the engine has no YouTube setup.
pub const NO_YOUTUBE_CLIENT: &str =
    "YouTube's API features are off: this build has no Google OAuth client";

/// What the host gives the engine for YouTube: the Google OAuth client, the HTTPS
/// transport, and where the refresh token is kept. The client is JStream's built-in one
/// ([`built_in`](Self::built_in), ADR-0058), or another one a developer overrides it
/// with ([`new`](Self::new)). While the built-in client is empty a host has no setup to
/// give, and YouTube's API features stay hidden.
pub struct YoutubeSetup {
    client_id: String,
    client_secret: Option<Secret>,
    transport: Arc<dyn HttpTransport>,
    tokens: Arc<dyn OAuthTokenStore>,
    sign_in_timeout: Duration,
    health_interval: Duration,
    stats_interval: Duration,
    daily_quota: u64,
    clock: WallClock,
    secret_hook: Option<SecretHook>,
}

impl fmt::Debug for YoutubeSetup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("YoutubeSetup")
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret)
            .field("sign_in_timeout", &self.sign_in_timeout)
            .field("health_interval", &self.health_interval)
            .field("stats_interval", &self.stats_interval)
            .field("daily_quota", &self.daily_quota)
            .finish_non_exhaustive()
    }
}

impl YoutubeSetup {
    /// JStream's built-in client ([`jstream_youtube::BUILT_IN_CLIENT_ID`]), with the
    /// transport and token store as in [`new`](Self::new). `None` while the built-in
    /// client is empty, as it is until the maintainer fills it in (ADR-0058 §1).
    pub fn built_in(
        transport: Arc<dyn HttpTransport>,
        tokens: Arc<dyn OAuthTokenStore>,
    ) -> Option<Self> {
        Self::from_constants(
            BUILT_IN_CLIENT_ID,
            BUILT_IN_CLIENT_SECRET,
            transport,
            tokens,
        )
    }

    /// A setup from a client ID and secret as constants hold them: `None` for an empty
    /// ID, and no secret for an empty one.
    pub(crate) fn from_constants(
        client_id: &str,
        client_secret: &str,
        transport: Arc<dyn HttpTransport>,
        tokens: Arc<dyn OAuthTokenStore>,
    ) -> Option<Self> {
        (!client_id.is_empty()).then(|| {
            let secret = (!client_secret.is_empty()).then(|| Secret::new(client_secret));
            Self::new(client_id, secret, transport, tokens)
        })
    }

    /// A Google OAuth client (`client_id`, and its secret if it has one; an installed
    /// app's can't really be kept secret, but it never prints), the transport
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
            stats_interval: STATS_INTERVAL,
            daily_quota: DAILY_QUOTA,
            clock: Arc::new(SystemTime::now),
            secret_hook: None,
        }
    }

    /// Hands `hook` every secret the account's client holds or gets: the refresh token,
    /// each access token and each stream key, such as the one a `youtube` output
    /// publishes with. A host registers them with its crash reports' redaction (#353).
    #[must_use]
    pub fn with_secret_hook(mut self, hook: SecretHook) -> Self {
        self.secret_hook = Some(hook);
        self
    }

    /// `client`, with the secret hook if there is one.
    fn hooked(&self, client: LiveClient) -> LiveClient {
        match &self.secret_hook {
            Some(hook) => client.with_secret_hook(Arc::clone(hook)),
            None => client,
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

    /// Reads a running output's audience every `interval` instead of
    /// [`STATS_INTERVAL`].
    #[must_use]
    pub fn with_stats_interval(mut self, interval: Duration) -> Self {
        self.stats_interval = interval;
        self
    }

    /// Assumes the day's quota is `units` instead of [`DAILY_QUOTA`], for a Google Cloud
    /// project YouTube granted more (or less). Zero stops audience polling.
    #[must_use]
    pub fn with_daily_quota(mut self, units: u64) -> Self {
        self.daily_quota = units;
        self
    }

    /// Reads the quota's day from `clock` instead of the system clock, for tests.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_clock(mut self, clock: WallClock) -> Self {
        self.clock = clock;
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

/// A running output's health and audience poll. Dropping it ends the poll.
struct Watch {
    _stop: Sender<()>,
    /// Numbers the polls, so one that ends on its own forgets only itself.
    generation: u64,
}

/// The day's quota estimate.
#[derive(Default)]
struct Quota {
    /// The day it counts, in days since the epoch at [`QUOTA_DAY_OFFSET_SECS`].
    day: i64,
    /// Units charged that day.
    used: u64,
    /// YouTube refused a call that day for quota.
    exhausted: bool,
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
    /// One poll per running `youtube` output, and the number the next one gets.
    watches: Mutex<(BTreeMap<OutputId, Watch>, u64)>,
    /// Each running `youtube` output's last audience reading.
    stats: Mutex<BTreeMap<OutputId, AudienceStatsInfo>>,
    quota: Mutex<Quota>,
}

impl Youtube {
    /// Starts signed in when the store holds a refresh token from an earlier sign-in.
    pub(crate) fn new(setup: YoutubeSetup, emit: Emit) -> Self {
        let client = match setup.tokens.get(TOKEN_ACCOUNT) {
            Ok(token) => token.map(|token| {
                setup.hooked(LiveClient::new(
                    Arc::clone(&setup.transport),
                    setup.oauth(),
                    token,
                ))
            }),
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
            watches: Mutex::new((BTreeMap::new(), 0)),
            stats: Mutex::new(BTreeMap::new()),
            quota: Mutex::new(Quota::default()),
        }
    }

    /// Replaces how a `youtube` output connects, for tests.
    #[cfg(test)]
    pub(crate) fn set_connect(&self, connect: ConnectRtmp) {
        *lock(&self.connect) = connect;
    }

    /// Holds the client's mutex, as a Live API call in flight does, for tests.
    #[cfg(test)]
    pub(crate) fn hold_client(&self) -> std::sync::MutexGuard<'_, Option<LiveClient>> {
        lock(&self.client)
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
                let code = redirect.wait(&pending, timeout);
                // A sign-out abandoned this sign-in: don't trade its code for a token.
                if code.is_ok() && !this.waiting_for(generation) {
                    return;
                }
                let outcome = code.and_then(|code| {
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

    /// Whether sign-in `generation` is the one waiting, not abandoned or replaced.
    fn waiting_for(&self, generation: u64) -> bool {
        lock(&self.auth).pending.as_ref().map(|p| p.generation) == Some(generation)
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
            drop(auth);
            // Abandoned while the code was exchanged: the token it got is unwanted.
            if let Ok(grant) = outcome
                && let Err(err) = self
                    .setup
                    .oauth()
                    .revoke(&*self.setup.transport, &grant.refresh)
            {
                tracing::warn!(error = %err, "revoking an abandoned YouTube sign-in's token");
            }
            return;
        }
        auth.pending = None;
        let mut client = lock(&self.client);
        // The account this sign-in replaces, if any: its token is revoked below.
        let mut replaced = None;
        // Logged once the locks are released.
        let failure = match outcome {
            Ok(grant) => match self.setup.tokens.get(TOKEN_ACCOUNT).and_then(|old| {
                self.setup
                    .tokens
                    .set(TOKEN_ACCOUNT, &grant.refresh)
                    .map(|()| old)
            }) {
                Ok(old) => {
                    replaced = old.filter(|old| old.expose() != grant.refresh.expose());
                    *client = Some(self.setup.hooked(LiveClient::from_grant(
                        Arc::clone(&self.setup.transport),
                        self.setup.oauth(),
                        grant,
                    )));
                    None
                }
                Err(err) => Some(format!(
                    "the credential store did not keep the YouTube token: {err}"
                )),
            },
            Err(err) => Some(err.to_string()),
        };
        // A failed new sign-in leaves the account that was signed in, if any.
        auth.state = if client.is_some() {
            YoutubeAuth::SignedIn
        } else {
            YoutubeAuth::SignedOut
        };
        let state = auth.state;
        drop(client);
        drop(auth);
        match failure {
            None => tracing::info!("YouTube sign-in finished"),
            Some(reason) => tracing::warn!(error = %reason, "YouTube sign-in did not finish"),
        }
        (self.emit)(Event::YoutubeStatusChanged { auth: state });
        if let Some(old) = replaced
            && let Err(err) = self.setup.oauth().revoke(&*self.setup.transport, &old)
        {
            tracing::warn!(error = %err, "revoking the replaced YouTube account's token");
        }
    }

    /// `youtube.sign_out`: forgets the client and the stored token, abandons a waiting
    /// sign-in, then revokes the token at Google (best effort). Waits for a call in
    /// flight, so a refresh token it rotates is the one cleared and revoked.
    pub(crate) fn sign_out(&self) -> Result<(), ProtocolError> {
        let token = {
            let mut auth = lock(&self.auth);
            let mut client = lock(&self.client);
            let token = self
                .setup
                .tokens
                .get(TOKEN_ACCOUNT)
                .and_then(|token| self.setup.tokens.clear(TOKEN_ACCOUNT).map(|()| token))
                .map_err(|err| ProtocolError::ActionFailed {
                    reason: format!("the credential store did not remove the YouTube token: {err}"),
                })?;
            *client = None;
            auth.pending = None;
            auth.generation += 1;
            auth.state = YoutubeAuth::SignedOut;
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
        let (state, cleared) = {
            let mut auth = lock(&self.auth);
            *lock(&self.client) = None;
            let cleared = self.setup.tokens.clear(TOKEN_ACCOUNT);
            if auth.pending.is_none() {
                auth.state = YoutubeAuth::SignedOut;
            }
            (auth.state, cleared)
        };
        tracing::warn!("YouTube no longer accepts the sign-in");
        if let Err(err) = cleared {
            tracing::error!(error = %err, "removing the refused YouTube token");
        }
        (self.emit)(Event::YoutubeStatusChanged { auth: state });
    }

    /// The day the quota estimate counts now.
    fn quota_day(&self) -> i64 {
        let now = (self.setup.clock)();
        let secs = match now.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(since) => i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
            Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
        };
        secs.saturating_add(QUOTA_DAY_OFFSET_SECS)
            .div_euclid(24 * 60 * 60)
    }

    /// The quota estimate, started again if its day has ended.
    fn quota(&self) -> std::sync::MutexGuard<'_, Quota> {
        let day = self.quota_day();
        let mut quota = lock(&self.quota);
        if quota.day != day {
            *quota = Quota {
                day,
                ..Quota::default()
            };
        }
        quota
    }

    /// Charges `units` to the day's estimate.
    fn charge(&self, units: u64) {
        let mut quota = self.quota();
        quota.used = quota.used.saturating_add(units);
    }

    /// Whether YouTube refused a call today for quota.
    fn quota_exhausted(&self) -> bool {
        self.quota().exhausted
    }

    /// How audience stats are polled at the quota left now.
    fn stats_polling(&self) -> StatsPolling {
        let quota = self.quota();
        let daily = self.setup.daily_quota;
        let left = daily.saturating_sub(quota.used);
        // A day of no quota allows no reads at all (review N2).
        if quota.exhausted || daily == 0 || left.saturating_mul(10) < daily {
            StatsPolling::Stopped
        } else if left.saturating_mul(5) < daily {
            StatsPolling::Slowed
        } else {
            StatsPolling::Normal
        }
    }

    /// `youtube.stats`: the last audience reading of each running output and the day's
    /// quota estimate. Never calls YouTube.
    pub(crate) fn audience(&self) -> QueryResult {
        let outputs = lock(&self.stats).values().cloned().collect();
        let stats = self.stats_polling();
        let quota = self.quota();
        QueryResult::YoutubeStats {
            outputs,
            quota: QuotaInfo {
                used: quota.used,
                daily: self.setup.daily_quota,
                exhausted: quota.exhausted,
                stats,
            },
        }
    }

    /// Runs `call` on the signed-in client, charging `units` to the day's quota first,
    /// then stores a rotated refresh token.
    fn with_client<T>(
        &self,
        units: u64,
        call: impl FnOnce(&mut LiveClient) -> Result<T, YoutubeError>,
    ) -> Result<T, ProtocolError> {
        // The rotated token is stored before the client is released, so a sign-out or a
        // new sign-in waiting for the client always has the last word in the store.
        let (outcome, kept) = {
            let mut client = lock(&self.client);
            let Some(client) = client.as_mut() else {
                return Err(ProtocolError::ActionFailed {
                    reason: "not signed in to YouTube: send `youtube.sign_in`".into(),
                });
            };
            self.charge(units);
            let outcome = call(client);
            let kept = client
                .take_rotated_refresh_token()
                .map(|token| self.setup.tokens.set(TOKEN_ACCOUNT, &token));
            (outcome, kept)
        };
        if let Some(Err(err)) = kept {
            tracing::error!(error = %err, "the credential store did not keep the rotated YouTube token");
        }
        if let Err(err) = &outcome
            && err.is_quota()
        {
            self.quota().exhausted = true;
            tracing::warn!("YouTube's API quota is used up for the day");
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
        let page = self.with_client(READ_UNITS, |client| {
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
        let page = self.with_client(READ_UNITS, |client| {
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
        let broadcast = self.with_client(WRITE_UNITS, |client| client.insert_broadcast(&new))?;
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
        // Without a stream: the broadcast's read, the stream's insert and the bind.
        let units = if stream_id.is_some() {
            WRITE_UNITS
        } else {
            READ_UNITS + 2 * WRITE_UNITS
        };
        let broadcast = self.with_client(units, |client| {
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
        let broadcast =
            self.with_client(WRITE_UNITS, |client| client.transition(broadcast_id, to))?;
        Ok(ActionResult::YoutubeTransitionBroadcast {
            broadcast: broadcast_info(broadcast),
        })
    }

    /// The `rtmps://` ingest of `broadcast_id`'s bound stream, with its key.
    pub(crate) fn ingest(&self, broadcast_id: &str) -> Result<Ingest, String> {
        let found = self
            .with_client(2 * READ_UNITS, |client| {
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

    /// Starts reading the health of `stream_id` and the audience of `broadcast_id`
    /// while `alive` says `output_id` still runs: health every health interval, emitting
    /// `youtube.health_changed` when it changes, and the audience every stats interval
    /// (slower or not at all when the quota runs low), emitting `youtube.stats` when it
    /// changes. A poll already running for `output_id` ends; other outputs' polls keep
    /// running.
    pub(crate) fn watch(
        self: &Arc<Self>,
        output_id: OutputId,
        broadcast_id: String,
        stream_id: String,
        alive: Box<dyn Fn() -> bool + Send>,
    ) {
        let (stop, stopped) = mpsc::channel::<()>();
        let this = Arc::downgrade(self);
        let health_every = self.setup.health_interval;
        let stats_every = self.setup.stats_interval;
        let watched = output_id.clone();
        let mut watches = lock(&self.watches);
        let generation = watches.1;
        watches.1 += 1;
        let spawned = thread::Builder::new()
            .name("jstream-youtube-health".into())
            .spawn(move || {
                let mut last = None;
                let start = Instant::now();
                let mut next_health = start + health_every;
                let mut next_stats = start + stats_every;
                loop {
                    let wait = next_health
                        .min(next_stats)
                        .saturating_duration_since(Instant::now());
                    match stopped.recv_timeout(wait) {
                        Err(RecvTimeoutError::Timeout) => {}
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                    }
                    let Some(youtube) = this.upgrade() else {
                        return;
                    };
                    if !alive() {
                        youtube.watch_ended(&watched, generation);
                        return;
                    }
                    let now = Instant::now();
                    let health_due = now >= next_health;
                    let stats_due = now >= next_stats;
                    let polling = youtube.stats_polling();
                    if health_due {
                        next_health = now + health_every;
                    }
                    if stats_due {
                        next_stats = now
                            + match polling {
                                StatsPolling::Slowed => stats_every * STATS_SLOWDOWN,
                                _ => stats_every,
                            };
                    }
                    // Signed out while the output runs: wait quietly for a sign-in.
                    if !youtube.signed_in() || youtube.quota_exhausted() {
                        continue;
                    }
                    if health_due {
                        match youtube
                            .with_client(READ_UNITS, |client| client.stream_health(&stream_id))
                        {
                            Ok(health) => {
                                let event =
                                    health_event(&watched, &broadcast_id, &stream_id, health);
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
                    if stats_due && polling != StatsPolling::Stopped {
                        youtube.read_audience(&watched, generation, &broadcast_id);
                    }
                }
            });
        match spawned {
            Ok(_) => {
                watches.0.insert(
                    output_id.clone(),
                    Watch {
                        _stop: stop,
                        generation,
                    },
                );
                // A new poll starts with no reading.
                lock(&self.stats).remove(&output_id);
            }
            Err(err) => {
                tracing::error!(output_id = %output_id, error = %err, "starting the YouTube health poll");
            }
        }
    }

    /// Reads `broadcast_id`'s audience for poll `generation` of `output_id`, keeps it,
    /// and emits `youtube.stats` when it differs from the last reading.
    fn read_audience(&self, output_id: &OutputId, generation: u64, broadcast_id: &str) {
        let read = match self.with_client(READ_UNITS, |client| client.audience_stats(broadcast_id))
        {
            Ok(read) => read,
            Err(err) => {
                tracing::warn!(output_id = %output_id, error = %err, "reading the YouTube audience");
                return;
            }
        };
        let stats = AudienceStatsInfo {
            output_id: output_id.clone(),
            broadcast_id: broadcast_id.to_owned(),
            concurrent_viewers: read.concurrent_viewers,
            likes: read.likes,
            views: read.views,
        };
        {
            // Kept only while this poll is the output's: a stop during the read wins.
            let watches = lock(&self.watches);
            if watches.0.get(output_id).map(|watch| watch.generation) != Some(generation) {
                return;
            }
            let mut kept = lock(&self.stats);
            if kept.get(output_id) == Some(&stats) {
                return;
            }
            kept.insert(output_id.clone(), stats.clone());
        }
        // Emitted with the locks dropped, so the sink never runs under them: a stop that
        // lands just now has already removed the reading, and this one event still goes
        // out after it (review N1).
        (self.emit)(Event::YoutubeStats { stats });
    }

    /// Poll `generation` of `output_id` saw the output stop: forget it, unless a newer
    /// poll has replaced it.
    fn watch_ended(&self, output_id: &OutputId, generation: u64) {
        let mut watches = lock(&self.watches);
        if watches.0.get(output_id).map(|watch| watch.generation) == Some(generation) {
            watches.0.remove(output_id);
            lock(&self.stats).remove(output_id);
        }
    }

    /// Ends the poll of `output_id`, if one runs, and forgets its audience. The thread
    /// notices at once, unless a read is in flight, and is not waited for.
    pub(crate) fn unwatch(&self, output_id: &OutputId) {
        let mut watches = lock(&self.watches);
        watches.0.remove(output_id);
        lock(&self.stats).remove(output_id);
    }
}

/// A `jstream-youtube` error as a protocol error. Quota errors say so first
/// (ADR-0041 §5, kept by ADR-0058).
fn failed(err: YoutubeError) -> ProtocolError {
    let reason = if err.is_quota() {
        format!("YouTube's API quota is used up: {err}")
    } else {
        format!("YouTube: {err}")
    };
    ProtocolError::ActionFailed { reason }
}

fn invalid(field: &str, reason: &str) -> ProtocolError {
    ProtocolError::invalid_argument(field, reason)
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
