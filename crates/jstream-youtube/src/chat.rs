//! Reading a broadcast's live chat: [`LiveClient::chat_messages`] fetches one page of
//! `liveChatMessages.list`, and [`ChatPoller`] keeps polling it at the interval the API
//! asks for, never faster (ADR-0041 §5).
//!
//! Message text and author names come from strangers. They are kept as plain text, with
//! control characters replaced and their length capped, and whatever draws them draws
//! them as text (ADR-0041 §2).

use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::http::{Body, Method};
use crate::live::{non_empty, u64_or_string};
use crate::{LiveClient, Scope, YoutubeError};

/// The most messages one page asks for (the API's limit).
pub const MAX_CHAT_PAGE: u16 = 2000;

/// The shortest wait between polls, whatever the API returns. The API's own interval
/// is normally longer; this only guards against a zero or missing one.
pub const MIN_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The wait before the first retry after an error other than quota. It doubles on each
/// error in a row, up to [`MAX_ERROR_BACKOFF`].
pub const ERROR_BACKOFF: Duration = Duration::from_secs(5);

/// The longest wait after errors other than quota.
pub const MAX_ERROR_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// The wait before the first retry after a quota or rate-limit error. It doubles on each
/// quota error in a row, up to [`MAX_QUOTA_BACKOFF`].
pub const QUOTA_BACKOFF: Duration = Duration::from_secs(60);

/// The longest wait after quota errors.
pub const MAX_QUOTA_BACKOFF: Duration = Duration::from_secs(30 * 60);

/// The most characters of a message's text kept.
pub const MAX_MESSAGE_CHARS: usize = 1000;

/// The most characters of an author's name kept.
pub const MAX_AUTHOR_CHARS: usize = 100;

/// API error reasons that mean the chat is over or was never there, so polling stops.
const ENDED_REASONS: &[&str] = &["liveChatEnded", "liveChatNotFound", "liveChatDisabled"];

impl LiveClient {
    /// One page of a live chat's messages (`liveChatMessages.list`), oldest first. Pass
    /// the previous page's `next_page_token` to get only what arrived since; without
    /// one, the API returns the recent backlog. Needs [`Scope::Readonly`].
    ///
    /// # Errors
    /// [`YoutubeError::InvalidArgument`] for an empty chat ID; API errors such as
    /// `liveChatEnded`, `liveChatNotFound` and quota errors; OAuth and transport errors.
    pub fn chat_messages(
        &mut self,
        live_chat_id: &str,
        page_token: Option<&str>,
    ) -> Result<ChatPage, YoutubeError> {
        let mut query = vec![
            ("liveChatId", non_empty(live_chat_id)?),
            ("part", "id,snippet,authorDetails".to_owned()),
            ("maxResults", MAX_CHAT_PAGE.to_string()),
        ];
        if let Some(token) = page_token {
            query.push(("pageToken", token.to_owned()));
        }
        let raw: RawPage = self.call(
            Method::Get,
            "liveChat/messages",
            &query,
            Body::Empty,
            "liveChatMessages.list",
            Scope::Readonly,
        )?;
        Ok(raw.into())
    }
}

/// One page of a live chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatPage {
    /// The messages, oldest first.
    pub messages: Vec<ChatMessage>,
    /// The token that asks for the messages after these.
    pub next_page_token: Option<String>,
    /// How long the API asks to wait before the next page. `None` when it sent none.
    pub polling_interval: Option<Duration>,
    /// When the chat went offline (RFC 3339), once it has. No more messages will come.
    pub offline_at: Option<String>,
}

/// A live chat message, with the fields JStream shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    /// The message's ID.
    pub id: String,
    /// When it was posted (RFC 3339).
    pub published_at: Option<String>,
    /// What kind of event it is.
    pub kind: ChatEvent,
    /// The text YouTube displays for it (`displayMessage`): the message, a Super Chat's
    /// comment, or YouTube's description of an event. Plain text, control characters
    /// replaced, at most [`MAX_MESSAGE_CHARS`] characters.
    pub text: String,
    /// Who posted it.
    pub author: ChatAuthor,
}

/// A chat message's `snippet.type`, with the details JStream uses.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChatEvent {
    /// An ordinary message.
    Text,
    /// A Super Chat: a paid message.
    SuperChat(Paid),
    /// A Super Sticker: a paid sticker.
    SuperSticker(Paid),
    /// A new member, or a member's milestone message.
    Membership,
    /// A moderator removed the message with this ID.
    Deleted {
        /// The removed message's ID.
        message_id: String,
    },
    /// An event this crate doesn't read, such as a ban or a poll, by its `type`.
    Other(String),
}

/// What a Super Chat or Super Sticker paid.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Paid {
    /// The amount as YouTube displays it, such as `$5.00`.
    pub amount: String,
    /// The currency's ISO 4217 code.
    pub currency: String,
    /// The amount in millionths of the currency's unit.
    pub amount_micros: Option<u64>,
}

/// A chat message's author (`authorDetails`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatAuthor {
    /// The author's channel ID.
    pub channel_id: String,
    /// The display name. Plain text, control characters replaced, at most
    /// [`MAX_AUTHOR_CHARS`] characters.
    pub name: String,
    /// The channel that owns the chat.
    pub is_owner: bool,
    /// A moderator of the chat.
    pub is_moderator: bool,
    /// A member (sponsor) of the channel.
    pub is_member: bool,
    /// A verified channel.
    pub is_verified: bool,
}

/// What [`ChatPoller::poll`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Poll {
    /// It fetched a page. The messages are new since the last page, and may be none.
    Messages(Vec<ChatMessage>),
    /// It is too soon: the next poll may go at this instant. Nothing was sent.
    NotYet(Instant),
    /// The chat is over (offline, ended, disabled or gone). Polling stops for good.
    Ended,
}

/// Polls one live chat, keeping its page token and its pace.
///
/// The caller's loop calls [`poll`](Self::poll) and sleeps until [`due`](Self::due).
/// After a page, the next poll waits the interval the API asked for (at least
/// [`MIN_POLL_INTERVAL`]). After an error it backs off, doubling: from
/// [`QUOTA_BACKOFF`] for quota and rate-limit errors, from [`ERROR_BACKOFF`] for others.
/// Time comes in as an argument, so tests drive it.
#[derive(Clone, Debug)]
pub struct ChatPoller {
    live_chat_id: String,
    page_token: Option<String>,
    due: Instant,
    /// Errors in a row of the same class, quota or not.
    errors: u32,
    /// Whether those errors were quota errors.
    quota: bool,
    ended: bool,
}

impl ChatPoller {
    /// A poller for the chat `live_chat_id` names (a broadcast's
    /// `snippet.live_chat_id`), due at once. The first page is the chat's recent
    /// backlog.
    pub fn new(live_chat_id: impl Into<String>, now: Instant) -> Self {
        Self {
            live_chat_id: live_chat_id.into(),
            page_token: None,
            due: now,
            errors: 0,
            quota: false,
            ended: false,
        }
    }

    /// The chat this polls.
    pub fn live_chat_id(&self) -> &str {
        &self.live_chat_id
    }

    /// When the next poll may go.
    pub fn due(&self) -> Instant {
        self.due
    }

    /// Whether the chat is over.
    pub fn is_ended(&self) -> bool {
        self.ended
    }

    /// Fetches the next page if it is due at `now`.
    ///
    /// # Errors
    /// The error of a failed request. The poller has already scheduled its retry, so
    /// the caller reports the error and carries on; [`YoutubeError::is_quota`] tells a
    /// quota error, which the engine surfaces as a protocol error (ADR-0041 §5).
    /// [`YoutubeError::SignInRequired`] needs the user, so polling it again only fails
    /// again.
    pub fn poll(&mut self, client: &mut LiveClient, now: Instant) -> Result<Poll, YoutubeError> {
        if self.ended {
            return Ok(Poll::Ended);
        }
        if now < self.due {
            return Ok(Poll::NotYet(self.due));
        }
        match client.chat_messages(&self.live_chat_id, self.page_token.as_deref()) {
            Ok(page) => {
                self.errors = 0;
                if page.offline_at.is_some() {
                    self.ended = true;
                }
                if let Some(token) = page.next_page_token {
                    self.page_token = Some(token);
                }
                let wait = page
                    .polling_interval
                    .unwrap_or(MIN_POLL_INTERVAL)
                    .max(MIN_POLL_INTERVAL);
                self.due = now + wait;
                if self.ended && page.messages.is_empty() {
                    return Ok(Poll::Ended);
                }
                Ok(Poll::Messages(page.messages))
            }
            Err(e) if is_ended(&e) => {
                self.ended = true;
                Ok(Poll::Ended)
            }
            Err(e) => {
                // A change of class starts that class's ladder from its first step.
                if e.is_quota() != self.quota {
                    self.quota = e.is_quota();
                    self.errors = 0;
                }
                let (first, max) = if self.quota {
                    (QUOTA_BACKOFF, MAX_QUOTA_BACKOFF)
                } else {
                    (ERROR_BACKOFF, MAX_ERROR_BACKOFF)
                };
                let doubled = first.saturating_mul(1u32 << self.errors.min(16));
                self.errors = self.errors.saturating_add(1);
                self.due = now + doubled.min(max);
                Err(e)
            }
        }
    }
}

fn is_ended(e: &YoutubeError) -> bool {
    matches!(e, YoutubeError::Api { reason: Some(r), .. } if ENDED_REASONS.contains(&r.as_str()))
}

/// `text` as plain text: control characters replaced with spaces, cut to `max`
/// characters.
fn plain(text: &str, max: usize) -> String {
    text.chars()
        .take(max)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPage {
    #[serde(default)]
    items: Vec<RawMessage>,
    #[serde(default)]
    next_page_token: Option<String>,
    #[serde(default, deserialize_with = "u64_or_string")]
    polling_interval_millis: Option<u64>,
    #[serde(default)]
    offline_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMessage {
    id: String,
    #[serde(default)]
    snippet: RawSnippet,
    #[serde(default)]
    author_details: RawAuthor,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSnippet {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    display_message: String,
    #[serde(default)]
    super_chat_details: Option<RawPaid>,
    #[serde(default)]
    super_sticker_details: Option<RawPaid>,
    #[serde(default)]
    message_deleted_details: Option<RawDeleted>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPaid {
    #[serde(default)]
    amount_display_string: String,
    #[serde(default)]
    currency: String,
    #[serde(default, deserialize_with = "u64_or_string")]
    amount_micros: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDeleted {
    #[serde(default)]
    deleted_message_id: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAuthor {
    #[serde(default)]
    channel_id: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    is_chat_owner: bool,
    #[serde(default)]
    is_chat_moderator: bool,
    #[serde(default)]
    is_chat_sponsor: bool,
    #[serde(default)]
    is_verified: bool,
}

impl From<RawPage> for ChatPage {
    fn from(raw: RawPage) -> Self {
        Self {
            messages: raw.items.into_iter().map(ChatMessage::from).collect(),
            next_page_token: raw.next_page_token,
            polling_interval: raw.polling_interval_millis.map(Duration::from_millis),
            offline_at: raw.offline_at,
        }
    }
}

impl From<RawPaid> for Paid {
    fn from(raw: RawPaid) -> Self {
        Self {
            amount: plain(&raw.amount_display_string, 32),
            currency: plain(&raw.currency, 8),
            amount_micros: raw.amount_micros,
        }
    }
}

impl From<RawMessage> for ChatMessage {
    fn from(raw: RawMessage) -> Self {
        let s = raw.snippet;
        let kind = match s.kind.as_str() {
            "textMessageEvent" => ChatEvent::Text,
            "superChatEvent" => {
                ChatEvent::SuperChat(s.super_chat_details.unwrap_or_default().into())
            }
            "superStickerEvent" => {
                ChatEvent::SuperSticker(s.super_sticker_details.unwrap_or_default().into())
            }
            "newSponsorEvent" | "memberMilestoneChatEvent" => ChatEvent::Membership,
            "messageDeletedEvent" => ChatEvent::Deleted {
                message_id: s
                    .message_deleted_details
                    .map(|d| d.deleted_message_id)
                    .unwrap_or_default(),
            },
            other => ChatEvent::Other(plain(other, 64)),
        };
        let a = raw.author_details;
        Self {
            id: raw.id,
            published_at: s.published_at,
            kind,
            text: plain(&s.display_message, MAX_MESSAGE_CHARS),
            author: ChatAuthor {
                channel_id: a.channel_id,
                name: plain(&a.display_name, MAX_AUTHOR_CHARS),
                is_owner: a.is_chat_owner,
                is_moderator: a.is_chat_moderator,
                is_member: a.is_chat_sponsor,
                is_verified: a.is_verified,
            },
        }
    }
}

#[cfg(test)]
mod tests;
