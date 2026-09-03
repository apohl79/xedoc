use crate::message_processor::ConnectionSessionState;
use crate::outgoing_message::OutgoingEnvelope;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use xedoc_app_server_protocol::ExperimentalApi;
use xedoc_app_server_protocol::ServerRequest;

pub use xedoc_app_server_transport::AppServerTransport;
pub(crate) use xedoc_app_server_transport::CHANNEL_CAPACITY;
pub(crate) use xedoc_app_server_transport::ConnectionId;
pub(crate) use xedoc_app_server_transport::ConnectionOrigin;
pub(crate) use xedoc_app_server_transport::OutgoingMessage;
pub(crate) use xedoc_app_server_transport::QueuedOutgoingMessage;
pub(crate) use xedoc_app_server_transport::TransportEvent;
pub(crate) use xedoc_app_server_transport::acquire_app_server_startup_lock;
pub use xedoc_app_server_transport::app_server_control_socket_path;
pub(crate) use xedoc_app_server_transport::app_server_startup_lock_path;
pub use xedoc_app_server_transport::auth;
pub(crate) use xedoc_app_server_transport::prepare_control_socket_path;
pub(crate) use xedoc_app_server_transport::start_control_socket_acceptor;
pub(crate) use xedoc_app_server_transport::start_stdio_connection;
pub(crate) use xedoc_app_server_transport::start_websocket_acceptor;

const OUTBOUND_QUEUE_BYTE_CAPACITY: usize = 16 * 1024 * 1024;

pub(crate) struct ConnectionState {
    pub(crate) outbound_initialized: Arc<AtomicBool>,
    pub(crate) outbound_experimental_api_enabled: Arc<AtomicBool>,
    pub(crate) outbound_opted_out_notification_methods: Arc<RwLock<HashSet<String>>>,
    pub(crate) session: Arc<ConnectionSessionState>,
}

impl ConnectionState {
    pub(crate) fn new(
        _origin: ConnectionOrigin,
        outbound_initialized: Arc<AtomicBool>,
        outbound_experimental_api_enabled: Arc<AtomicBool>,
        outbound_opted_out_notification_methods: Arc<RwLock<HashSet<String>>>,
    ) -> Self {
        Self {
            outbound_initialized,
            outbound_experimental_api_enabled,
            outbound_opted_out_notification_methods,
            session: Arc::new(ConnectionSessionState::new()),
        }
    }
}

pub(crate) struct OutboundConnectionState {
    origin: ConnectionOrigin,
    pub(crate) initialized: Arc<AtomicBool>,
    pub(crate) experimental_api_enabled: Arc<AtomicBool>,
    pub(crate) opted_out_notification_methods: Arc<RwLock<HashSet<String>>>,
    pub(crate) writer: mpsc::Sender<QueuedOutgoingMessage>,
    disconnect_sender: Option<CancellationToken>,
    byte_budget: Arc<Semaphore>,
}

impl OutboundConnectionState {
    pub(crate) fn new(
        writer: mpsc::Sender<QueuedOutgoingMessage>,
        initialized: Arc<AtomicBool>,
        experimental_api_enabled: Arc<AtomicBool>,
        opted_out_notification_methods: Arc<RwLock<HashSet<String>>>,
        disconnect_sender: Option<CancellationToken>,
    ) -> Self {
        Self::new_with_origin(
            ConnectionOrigin::InProcess,
            writer,
            initialized,
            experimental_api_enabled,
            opted_out_notification_methods,
            disconnect_sender,
        )
    }

    pub(crate) fn new_with_origin(
        origin: ConnectionOrigin,
        writer: mpsc::Sender<QueuedOutgoingMessage>,
        initialized: Arc<AtomicBool>,
        experimental_api_enabled: Arc<AtomicBool>,
        opted_out_notification_methods: Arc<RwLock<HashSet<String>>>,
        disconnect_sender: Option<CancellationToken>,
    ) -> Self {
        Self {
            origin,
            initialized,
            experimental_api_enabled,
            opted_out_notification_methods,
            writer,
            disconnect_sender,
            byte_budget: Arc::new(Semaphore::new(OUTBOUND_QUEUE_BYTE_CAPACITY)),
        }
    }

    fn can_disconnect(&self) -> bool {
        self.origin != ConnectionOrigin::Stdio && self.disconnect_sender.is_some()
    }

    pub(crate) fn request_disconnect(&self) {
        if let Some(disconnect_sender) = &self.disconnect_sender {
            disconnect_sender.cancel();
        }
    }
}

fn should_skip_notification_for_connection(
    connection_state: &OutboundConnectionState,
    message: &OutgoingMessage,
) -> bool {
    let Ok(opted_out_notification_methods) = connection_state.opted_out_notification_methods.read()
    else {
        warn!("failed to read outbound opted-out notifications");
        return false;
    };
    match message {
        OutgoingMessage::AppServerNotification(envelope) => {
            if envelope.notification.experimental_reason().is_some()
                && !connection_state
                    .experimental_api_enabled
                    .load(Ordering::Acquire)
            {
                return true;
            }
            let method = envelope.notification.to_string();
            opted_out_notification_methods.contains(method.as_str())
        }
        _ => false,
    }
}

fn disconnect_connection(
    connections: &mut HashMap<ConnectionId, OutboundConnectionState>,
    connection_id: ConnectionId,
) -> bool {
    if let Some(connection_state) = connections.remove(&connection_id) {
        connection_state.request_disconnect();
        return true;
    }
    false
}

async fn send_message_to_connection(
    connections: &mut HashMap<ConnectionId, OutboundConnectionState>,
    connection_id: ConnectionId,
    message: OutgoingMessage,
    write_complete_tx: Option<tokio::sync::oneshot::Sender<()>>,
) -> bool {
    let Some(connection_state) = connections.get(&connection_id) else {
        warn!("dropping message for disconnected connection: {connection_id:?}");
        return false;
    };
    let message = filter_outgoing_message_for_connection(connection_state, message);
    if should_skip_notification_for_connection(connection_state, &message) {
        return false;
    }

    let writer = connection_state.writer.clone();
    let can_disconnect = connection_state.can_disconnect();
    let origin = connection_state.origin;
    let byte_budget = Arc::clone(&connection_state.byte_budget);
    let queued_message = if origin == ConnectionOrigin::InProcess {
        let mut queued_message = QueuedOutgoingMessage::new(message);
        queued_message.write_complete_tx = write_complete_tx;
        queued_message
    } else {
        let serialized_json = match serde_json::to_string(&message) {
            Ok(serialized_json) => serialized_json,
            Err(err) => {
                warn!("dropping outbound message that failed serialization: {err}");
                return false;
            }
        };
        if serialized_json.len() > OUTBOUND_QUEUE_BYTE_CAPACITY {
            warn!(
                "disconnecting connection after a single outbound message exceeded the byte budget: {connection_id:?}"
            );
            return disconnect_connection(connections, connection_id);
        }
        let permits = serialized_json.len().max(1) as u32;
        let byte_permit = if can_disconnect {
            match byte_budget.try_acquire_many_owned(permits) {
                Ok(byte_permit) => byte_permit,
                Err(_) => {
                    warn!(
                        "disconnecting slow connection after outbound byte budget filled: {connection_id:?}"
                    );
                    return disconnect_connection(connections, connection_id);
                }
            }
        } else {
            match byte_budget.acquire_many_owned(permits).await {
                Ok(byte_permit) => byte_permit,
                Err(_) => return disconnect_connection(connections, connection_id),
            }
        };
        QueuedOutgoingMessage::serialized(serialized_json, byte_permit, write_complete_tx)
    };
    if can_disconnect {
        match writer.try_send(queued_message) {
            Ok(()) => false,
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    "disconnecting slow connection after outbound queue filled: {connection_id:?}"
                );
                disconnect_connection(connections, connection_id)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                disconnect_connection(connections, connection_id)
            }
        }
    } else if writer.send(queued_message).await.is_err() {
        disconnect_connection(connections, connection_id)
    } else {
        false
    }
}

fn filter_outgoing_message_for_connection(
    connection_state: &OutboundConnectionState,
    message: OutgoingMessage,
) -> OutgoingMessage {
    let experimental_api_enabled = connection_state
        .experimental_api_enabled
        .load(Ordering::Acquire);
    match message {
        OutgoingMessage::Request(ServerRequest::CommandExecutionRequestApproval {
            request_id,
            mut params,
        }) => {
            if !experimental_api_enabled {
                params.strip_experimental_fields();
            }
            OutgoingMessage::Request(ServerRequest::CommandExecutionRequestApproval {
                request_id,
                params,
            })
        }
        _ => message,
    }
}

pub(crate) async fn route_outgoing_envelope(
    connections: &mut HashMap<ConnectionId, OutboundConnectionState>,
    envelope: OutgoingEnvelope,
) {
    match envelope {
        OutgoingEnvelope::ToConnection {
            connection_id,
            message,
            write_complete_tx,
        } => {
            let _ =
                send_message_to_connection(connections, connection_id, message, write_complete_tx)
                    .await;
        }
        OutgoingEnvelope::Broadcast { message } => {
            let target_connections: Vec<ConnectionId> = connections
                .iter()
                .filter_map(|(connection_id, connection_state)| {
                    if connection_state.initialized.load(Ordering::Acquire)
                        && !should_skip_notification_for_connection(connection_state, &message)
                    {
                        Some(*connection_id)
                    } else {
                        None
                    }
                })
                .collect();

            for connection_id in target_connections {
                let _ = send_message_to_connection(
                    connections,
                    connection_id,
                    message.clone(),
                    /*write_complete_tx*/ None,
                )
                .await;
            }
        }
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
