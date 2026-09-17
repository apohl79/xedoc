use super::CHANNEL_CAPACITY;
use super::ConnectionOrigin;
use super::SessionScriptConnectionScope;
use super::TransportEvent;
use super::allocator_pressure::release_after_large_write;
use super::next_connection_id;
use super::serialize_outgoing_message;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::QueuedOutgoingMessage;
use std::io::ErrorKind;
use std::io::Result as IoResult;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

/// Maximum JSON-RPC payload size accepted from a SessionScript child.
pub const MAX_SESSION_SCRIPT_FRAME_BYTES: usize = 1024 * 1024;

/// Attach a SessionScript child process's stdin and stdout as an app-server
/// JSONL connection.
///
/// The host owns process creation and termination. The supplied cancellation
/// token is used both as the child lifecycle signal and as the connection's
/// disconnect handle. The returned task completes after both pipe tasks stop
/// and the normal `ConnectionClosed` event has been emitted.
pub async fn start_session_script_connection<R, W>(
    child_stdout: R,
    child_stdin: W,
    transport_event_tx: mpsc::Sender<TransportEvent>,
    scope: SessionScriptConnectionScope,
    cancellation_token: CancellationToken,
) -> IoResult<(ConnectionId, JoinHandle<()>)>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let connection_id = next_connection_id();
    let (writer_tx, writer_rx) = mpsc::channel::<QueuedOutgoingMessage>(CHANNEL_CAPACITY);
    transport_event_tx
        .send(TransportEvent::ConnectionOpened {
            connection_id,
            origin: ConnectionOrigin::SessionScript,
            session_script_scope: Some(scope),
            writer: writer_tx.clone(),
            disconnect_sender: Some(cancellation_token.clone()),
        })
        .await
        .map_err(|_| {
            std::io::Error::new(ErrorKind::BrokenPipe, "app-server processor unavailable")
        })?;

    let reader_writer_tx = writer_tx;
    let reader_event_tx = transport_event_tx.clone();
    let reader_token = cancellation_token.clone();
    let writer_token = cancellation_token.clone();
    let connection_event_tx = transport_event_tx;
    let task = tokio::spawn(async move {
        let mut reader_task = tokio::spawn(run_inbound(
            child_stdout,
            reader_event_tx,
            reader_writer_tx,
            connection_id,
            reader_token,
        ));
        let mut writer_task = tokio::spawn(run_outbound(child_stdin, writer_rx, writer_token));

        tokio::select! {
            _ = &mut reader_task => {
                cancellation_token.cancel();
                writer_task.abort();
            }
            _ = &mut writer_task => {
                cancellation_token.cancel();
                reader_task.abort();
            }
            _ = cancellation_token.cancelled() => {
                reader_task.abort();
                writer_task.abort();
            }
        }

        let _ = connection_event_tx
            .send(TransportEvent::ConnectionClosed { connection_id })
            .await;
        debug!(?connection_id, "session script child transport closed");
    });

    Ok((connection_id, task))
}

async fn run_inbound<R>(
    child_stdout: R,
    transport_event_tx: mpsc::Sender<TransportEvent>,
    writer: mpsc::Sender<QueuedOutgoingMessage>,
    connection_id: ConnectionId,
    cancellation_token: CancellationToken,
) where
    R: AsyncRead + Unpin,
{
    let mut reader = child_stdout;
    let mut chunk = [0_u8; 8 * 1024];
    let mut frame = Vec::with_capacity(8 * 1024);

    'read: loop {
        let bytes_read = tokio::select! {
            _ = cancellation_token.cancelled() => break,
            result = reader.read(&mut chunk) => match result {
                Ok(bytes_read) => bytes_read,
                Err(err) => {
                    error!("failed reading SessionScript child stdout: {err}");
                    break;
                }
            },
        };
        if bytes_read == 0 {
            if !frame.is_empty()
                && !forward_frame(&transport_event_tx, &writer, connection_id, &mut frame).await
            {
                break;
            }
            break;
        }

        let mut offset = 0;
        while offset < bytes_read {
            let remaining = &chunk[offset..bytes_read];
            let newline_offset = remaining.iter().position(|byte| *byte == b'\n');
            let bytes_before_newline = newline_offset.unwrap_or(remaining.len());
            if frame.len() + bytes_before_newline > MAX_SESSION_SCRIPT_FRAME_BYTES {
                warn!(
                    ?connection_id,
                    max_bytes = MAX_SESSION_SCRIPT_FRAME_BYTES,
                    "SessionScript child JSONL frame exceeded maximum size"
                );
                break 'read;
            }
            frame.extend_from_slice(&remaining[..bytes_before_newline]);

            if newline_offset.is_none() {
                break;
            }
            offset += bytes_before_newline + 1;
            if !forward_frame(&transport_event_tx, &writer, connection_id, &mut frame).await {
                break 'read;
            }
        }
    }
}

async fn forward_frame(
    transport_event_tx: &mpsc::Sender<TransportEvent>,
    writer: &mpsc::Sender<QueuedOutgoingMessage>,
    connection_id: ConnectionId,
    frame: &mut Vec<u8>,
) -> bool {
    if frame.last() == Some(&b'\r') {
        frame.pop();
    }
    let payload = match std::str::from_utf8(frame) {
        Ok(payload) => payload,
        Err(err) => {
            error!("failed decoding SessionScript child stdout as UTF-8: {err}");
            return false;
        }
    };
    let should_continue =
        super::forward_incoming_message(transport_event_tx, writer, connection_id, payload).await;
    frame.clear();
    should_continue
}

async fn run_outbound<W>(
    child_stdin: W,
    mut writer_rx: mpsc::Receiver<QueuedOutgoingMessage>,
    cancellation_token: CancellationToken,
) where
    W: AsyncWrite + Unpin,
{
    let mut writer = child_stdin;
    while let Some(mut queued_message) = tokio::select! {
        _ = cancellation_token.cancelled() => None,
        message = writer_rx.recv() => message,
    } {
        let Some(mut json) = queued_message.take_serialized_json().or_else(|| {
            queued_message
                .typed_message()
                .and_then(serialize_outgoing_message)
        }) else {
            continue;
        };
        let serialized_bytes = json.len();
        json.push('\n');
        if let Err(err) = writer.write_all(json.as_bytes()).await {
            error!("failed writing to SessionScript child stdin: {err}");
            break;
        }
        if let Some(write_complete_tx) = queued_message.write_complete_tx.take() {
            let _ = write_complete_tx.send(());
        }
        drop(json);
        drop(queued_message);
        release_after_large_write(serialized_bytes);
    }
    info!("SessionScript child stdin writer exited");
}
