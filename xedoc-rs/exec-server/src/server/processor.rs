use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::Instrument;
use tracing::debug;
use tracing::warn;

use crate::ExecServerRuntimePaths;
use crate::connection::CHANNEL_CAPACITY;
use crate::connection::JsonRpcConnection;
use crate::connection::JsonRpcConnectionEvent;
use crate::rpc::RpcNotificationSender;
use crate::rpc::RpcServerOutboundMessage;
use crate::rpc::encode_server_message;
use crate::rpc::invalid_request;
use crate::rpc::method_not_found;
use crate::server::ExecServerHandler;
use crate::server::registry::build_router;
use crate::server::session_registry::SessionRegistry;
use crate::telemetry::ConnectionTransport;
use crate::telemetry::ExecServerTelemetry;

#[derive(Clone)]
pub(crate) struct ConnectionProcessor {
    session_registry: Arc<SessionRegistry>,
    runtime_paths: ExecServerRuntimePaths,
    telemetry: ExecServerTelemetry,
}

impl ConnectionProcessor {
    #[cfg(test)]
    pub(crate) fn new(runtime_paths: ExecServerRuntimePaths) -> Self {
        Self::new_with_telemetry(runtime_paths, ExecServerTelemetry::default())
    }

    pub(crate) fn new_with_telemetry(
        runtime_paths: ExecServerRuntimePaths,
        telemetry: ExecServerTelemetry,
    ) -> Self {
        Self {
            session_registry: SessionRegistry::new(telemetry.clone()),
            runtime_paths,
            telemetry,
        }
    }

    pub(crate) async fn run_connection(
        &self,
        connection: JsonRpcConnection,
        transport: ConnectionTransport,
    ) {
        run_connection(
            connection,
            Arc::clone(&self.session_registry),
            self.runtime_paths.clone(),
            self.telemetry.clone(),
            transport,
        )
        .await;
    }

    pub(crate) async fn shutdown(&self) {
        self.session_registry.shutdown().await;
    }
}

async fn run_connection(
    connection: JsonRpcConnection,
    session_registry: Arc<SessionRegistry>,
    runtime_paths: ExecServerRuntimePaths,
    telemetry: ExecServerTelemetry,
    transport: ConnectionTransport,
) {
    let _connection_metrics = telemetry.connection_started(transport);
    let router = Arc::new(build_router());
    let JsonRpcConnection {
        outgoing_tx: json_outgoing_tx,
        mut incoming_rx,
        mut disconnected_rx,
        task_handles: connection_tasks,
        transport: _transport,
    } = connection;
    let (outgoing_tx, mut outgoing_rx) =
        mpsc::channel::<RpcServerOutboundMessage>(CHANNEL_CAPACITY);
    let notifications = RpcNotificationSender::new(outgoing_tx.clone());
    let handler = Arc::new(ExecServerHandler::new(
        session_registry,
        notifications,
        runtime_paths,
    ));

    let outbound_task = tokio::spawn(async move {
        while let Some(message) = outgoing_rx.recv().await {
            let json_message = match encode_server_message(message) {
                Ok(json_message) => json_message,
                Err(err) => {
                    warn!("failed to serialize exec-server outbound message: {err}");
                    break;
                }
            };
            if json_outgoing_tx.send(json_message).await.is_err() {
                break;
            }
        }
    });

    // Process inbound events sequentially to preserve initialize/initialized ordering.
    while let Some(event) = incoming_rx.recv().await {
        if !handler.is_session_attached() {
            debug!("exec-server connection evicted after session resume");
            break;
        }
        match event {
            JsonRpcConnectionEvent::MalformedMessage { reason } => {
                warn!("ignoring malformed exec-server message: {reason}");
                if outgoing_tx
                    .send(RpcServerOutboundMessage::Error {
                        request_id: xedoc_exec_server_protocol::RequestId::Integer(-1),
                        error: invalid_request(reason),
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            JsonRpcConnectionEvent::Message(message) => match message {
                xedoc_exec_server_protocol::JSONRPCMessage::Request(request) => {
                    let request_started_at = Instant::now();
                    if let Some((method, route)) = router.request_route(request.method.as_str()) {
                        let request_span = request_span(method, &request);
                        let message = tokio::select! {
                            message = route(Arc::clone(&handler), request).instrument(request_span.clone()) => message,
                            _ = disconnected_rx.changed() => {
                                request_span.record("result", "disconnected");
                                telemetry.request_completed(
                                    method,
                                    "disconnected",
                                    request_started_at.elapsed(),
                                );
                                debug!("exec-server transport disconnected while handling request");
                                break;
                            }
                        };
                        let result = request_result(&message);
                        if let Some(message) = message
                            && outgoing_tx.send(message).await.is_err()
                        {
                            request_span.record("result", "disconnected");
                            telemetry.request_completed(
                                method,
                                "disconnected",
                                request_started_at.elapsed(),
                            );
                            break;
                        }
                        request_span.record("result", result);
                        telemetry.request_completed(method, result, request_started_at.elapsed());
                        drop(request_span);
                    } else {
                        let method = "unknown";
                        let request_span = request_span(method, &request);
                        if outgoing_tx
                            .send(RpcServerOutboundMessage::Error {
                                request_id: request.id,
                                error: method_not_found(format!(
                                    "exec-server stub does not implement `{}` yet",
                                    request.method
                                )),
                            })
                            .await
                            .is_err()
                        {
                            request_span.record("result", "disconnected");
                            telemetry.request_completed(
                                method,
                                "disconnected",
                                request_started_at.elapsed(),
                            );
                            break;
                        }
                        request_span.record("result", "error");
                        telemetry.request_completed(method, "error", request_started_at.elapsed());
                    }
                }
                xedoc_exec_server_protocol::JSONRPCMessage::Notification(notification) => {
                    let Some(route) = router.notification_route(notification.method.as_str())
                    else {
                        warn!(
                            "closing exec-server connection after unexpected notification: {}",
                            notification.method
                        );
                        break;
                    };
                    let result = tokio::select! {
                        result = route(Arc::clone(&handler), notification) => result,
                        _ = disconnected_rx.changed() => {
                            debug!(
                                "exec-server transport disconnected while handling notification"
                            );
                            break;
                        }
                    };
                    if let Err(err) = result {
                        warn!("closing exec-server connection after protocol error: {err}");
                        break;
                    }
                }
                xedoc_exec_server_protocol::JSONRPCMessage::Response(response) => {
                    warn!(
                        "closing exec-server connection after unexpected client response: {:?}",
                        response.id
                    );
                    break;
                }
                xedoc_exec_server_protocol::JSONRPCMessage::Error(error) => {
                    warn!(
                        "closing exec-server connection after unexpected client error: {:?}",
                        error.id
                    );
                    break;
                }
            },
            JsonRpcConnectionEvent::Disconnected { reason } => {
                if let Some(reason) = reason {
                    debug!("exec-server connection disconnected: {reason}");
                }
                break;
            }
        }
    }

    handler.shutdown().await;
    drop(handler);
    drop(outgoing_tx);
    for task in connection_tasks {
        task.abort();
        let _ = task.await;
    }
    let _ = outbound_task.await;
}

fn request_span(
    span_name: &str,
    request: &xedoc_exec_server_protocol::JSONRPCRequest,
) -> tracing::Span {
    let method = request.method.as_str();
    let span = tracing::info_span!(
        "xedoc.exec_server.request",
        otel.kind = "server",
        otel.name = span_name,
        method,
        result = tracing::field::Empty,
    );
    if let Some(trace) = &request.trace
        && !xedoc_otel::set_parent_from_w3c_trace_context(&span, trace)
    {
        warn!(method, "ignoring invalid inbound exec-server trace carrier");
    }
    span
}

fn request_result(message: &Option<RpcServerOutboundMessage>) -> &'static str {
    match message {
        Some(RpcServerOutboundMessage::Error { .. }) => "error",
        Some(
            RpcServerOutboundMessage::Response { .. } | RpcServerOutboundMessage::Notification(_),
        )
        | None => "success",
    }
}
