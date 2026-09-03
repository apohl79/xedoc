use std::fmt;

use serde::Serialize;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::oneshot;
use xedoc_app_server_protocol::JSONRPCErrorError;
use xedoc_app_server_protocol::RequestId;
use xedoc_app_server_protocol::Result;
use xedoc_app_server_protocol::ServerNotificationEnvelope;
use xedoc_app_server_protocol::ServerRequest;

/// Stable identifier for a transport connection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConnectionId(pub u64);

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Outgoing message from the server to the client.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum OutgoingMessage {
    Request(ServerRequest),
    /// AppServerNotification is specific to the case where this is run as an
    /// "app server" as opposed to an MCP server.
    AppServerNotification(ServerNotificationEnvelope),
    Response(OutgoingResponse),
    Error(OutgoingError),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutgoingResponse {
    pub id: RequestId,
    pub result: Result,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutgoingError {
    pub error: JSONRPCErrorError,
    pub id: RequestId,
}

#[derive(Debug)]
pub struct QueuedOutgoingMessage {
    pub write_complete_tx: Option<oneshot::Sender<()>>,
    payload: Option<QueuedOutgoingPayload>,
    _byte_permit: Option<OwnedSemaphorePermit>,
}

#[derive(Debug)]
enum QueuedOutgoingPayload {
    Typed(Box<OutgoingMessage>),
    Serialized(String),
}

impl QueuedOutgoingMessage {
    pub fn new(message: OutgoingMessage) -> Self {
        Self {
            write_complete_tx: None,
            payload: Some(QueuedOutgoingPayload::Typed(Box::new(message))),
            _byte_permit: None,
        }
    }

    pub fn serialized(
        serialized_json: String,
        byte_permit: OwnedSemaphorePermit,
        write_complete_tx: Option<oneshot::Sender<()>>,
    ) -> Self {
        Self {
            write_complete_tx,
            payload: Some(QueuedOutgoingPayload::Serialized(serialized_json)),
            _byte_permit: Some(byte_permit),
        }
    }

    pub fn into_typed_message(mut self) -> Option<OutgoingMessage> {
        self.take_typed_message()
    }

    pub fn take_typed_message(&mut self) -> Option<OutgoingMessage> {
        match self.payload.take()? {
            QueuedOutgoingPayload::Typed(message) => Some(*message),
            serialized @ QueuedOutgoingPayload::Serialized(_) => {
                self.payload = Some(serialized);
                None
            }
        }
    }

    pub(crate) fn typed_message(&self) -> Option<&OutgoingMessage> {
        match self.payload.as_ref()? {
            QueuedOutgoingPayload::Typed(message) => Some(message.as_ref()),
            QueuedOutgoingPayload::Serialized(_) => None,
        }
    }

    pub(crate) fn take_serialized_json(&mut self) -> Option<String> {
        match self.payload.take()? {
            QueuedOutgoingPayload::Serialized(serialized_json) => Some(serialized_json),
            typed @ QueuedOutgoingPayload::Typed(_) => {
                self.payload = Some(typed);
                None
            }
        }
    }
}
