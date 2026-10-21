//! Native renderer transport for the loopback session browser.

use std::io;
use std::time::Duration;

use futures::SinkExt;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::time::timeout;
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// Relays native renderer frames and browser input through an app-server capability.
///
/// # Errors
/// Returns an I/O error when the loopback connection closes or cannot keep up.
#[tracing::instrument(skip_all)]
pub async fn run_web_ui_bridge(
    url: String,
    mut frames: watch::Receiver<Option<String>>,
    inputs: mpsc::Sender<String>,
) -> io::Result<()> {
    let parsed = url::Url::parse(&url)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid web UI bridge URL"))?;
    if parsed.scheme() != "ws"
        || parsed.host_str() != Some("127.0.0.1")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !parsed.path().starts_with("/bridge/")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "web UI bridge must use loopback",
        ));
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(16 * 1024 * 1024))
        .max_frame_size(Some(16 * 1024 * 1024));
    let (mut socket, _) = timeout(
        Duration::from_secs(/*secs*/ 5),
        connect_async_with_config(&url, Some(config), /*disable_nagle*/ true),
    )
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "session web UI connection timed out",
        )
    })?
    .map_err(|_| io::Error::other("failed to connect session web UI"))?;
    let mut pending_frame = frames.borrow_and_update().clone();
    loop {
        if let Some(frame) = pending_frame.take() {
            timeout(
                Duration::from_secs(/*secs*/ 5),
                socket.send(Message::Text(frame.into())),
            )
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "session web UI stalled"))?
            .map_err(|_| io::Error::other("session web UI disconnected"))?;
        }
        tokio::select! {
            changed = frames.changed() => {
                if changed.is_err() { return Ok(()); }
                pending_frame = frames.borrow_and_update().clone();
            }
            input = socket.next() => {
                match input {
                    Some(Ok(Message::Text(text))) => {
                        inputs.send(text.to_string()).await
                            .map_err(|_| io::Error::other("session web UI input channel closed"))?;
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(Message::Binary(_) | Message::Frame(_) | Message::Close(_))) | Some(Err(_)) | None => {
                        return Err(io::Error::other("session web UI disconnected"));
                    }
                }
            }
        }
    }
}
