//! Asking the person before a new agent connects (0.108,
//! [ADR-0131](../../../docs/adr/0131-ask-before-a-new-agent-connects.md)).
//!
//! In the desktop app with MCP on, a connection that presents the control token from the
//! token file, and names in its `hello` a client the person hasn't approved, is held:
//! no `welcome` until the person answers. Every admitted client hears of it
//! (`agent.approval_requested`) and of its end (`agent.approval_resolved`), and any of
//! them may answer it (`agent.approve`). The approved list is read with `agent.approved`
//! and a row removed with `agent.forget`. All of this is a notice with a veto, not a
//! control: a client can pass by naming itself after an approved one (ADR-0131 §1).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How a held client reaches the server.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentTransport {
    /// The WebSocket API: a script, the CLI, or `jstream-cli mcp`, which carries an MCP
    /// client's stdio over it.
    Websocket,
    /// MCP's streamable HTTP transport, at the server's `/mcp`.
    McpHttp,
}

/// A client that asked to connect and waits for the person's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AgentApproval {
    /// Names this request, for `agent.approve`. Unique while the server runs.
    pub approval_id: String,
    /// The name the client gave in `hello`: its own text, never an instruction.
    pub client_name: String,
    /// How it connects.
    pub transport: AgentTransport,
}

impl AgentApproval {
    /// A request.
    pub fn new(
        approval_id: impl Into<String>,
        client_name: impl Into<String>,
        transport: AgentTransport,
    ) -> Self {
        AgentApproval {
            approval_id: approval_id.into(),
            client_name: client_name.into(),
            transport,
        }
    }
}

/// A row of the approved list: a client name and how it connects.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[non_exhaustive]
pub struct ApprovedAgent {
    /// The name the client gives in `hello`.
    pub client_name: String,
    /// How it connects.
    pub transport: AgentTransport,
}

impl ApprovedAgent {
    /// A row.
    pub fn new(client_name: impl Into<String>, transport: AgentTransport) -> Self {
        ApprovedAgent {
            client_name: client_name.into(),
            transport,
        }
    }
}

/// The person's answer to a held client (`agent.approve`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApprovalDecision {
    /// Let it in this time.
    AllowOnce,
    /// Let it in, and add it to the approved list.
    AlwaysAllow,
    /// Turn it away: its `hello` fails with `not_allowed`.
    Deny,
}

/// How a held client's request ended (`agent.approval_resolved`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApprovalOutcome {
    /// Allowed this time.
    AllowedOnce,
    /// Allowed, and added to the approved list.
    AlwaysAllowed,
    /// Denied.
    Denied,
    /// No one answered in time; it was turned away.
    TimedOut,
    /// The client went away first.
    Closed,
}
