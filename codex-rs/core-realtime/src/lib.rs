//! Realtime conversation transport and handoff buffering.
//!
//! Session configuration and event routing stay in codex-core; this crate owns the
//! WebSocket/WebRTC lifecycle and its bounded transport state.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use async_channel::Receiver;
use async_channel::RecvError;
use async_channel::Sender;
use async_channel::TrySendError;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use codex_api::ApiError;
use codex_api::Provider as ApiProvider;
use codex_api::RealtimeAudioFrame;
use codex_api::RealtimeContextAppendChannel;
use codex_api::RealtimeEvent;
use codex_api::RealtimeEventParser;
use codex_api::RealtimeSessionConfig;
use codex_api::RealtimeWebsocketClient;
use codex_api::RealtimeWebsocketEvents;
use codex_api::RealtimeWebsocketWriter;
use codex_api::map_api_error;
use codex_core_client::ModelClient;
use codex_core_realtime_context::truncate_realtime_text_to_token_budget;
use codex_login::default_client::default_headers;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::MessagePhase;
use codex_protocol::protocol::CodexResponseHandoffMode;
use codex_protocol::protocol::ConversationTextParams;
use codex_protocol::protocol::ConversationTextRole;
use codex_protocol::protocol::RealtimeTranscriptEntry;
use codex_utils_output_truncation::approx_bytes_for_tokens;
use codex_utils_string::take_bytes_at_char_boundary;
use http::HeaderMap;
use serde_json::json;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

mod bem;

use self::bem::ChannelParser as BemChannelParser;
use self::bem::message_phase as bem_message_phase;

const AUDIO_IN_QUEUE_CAPACITY: usize = 256;
const TEXT_IN_QUEUE_CAPACITY: usize = 64;
const HANDOFF_OUT_QUEUE_CAPACITY: usize = 64;
const OUTPUT_EVENTS_QUEUE_CAPACITY: usize = 256;
const REALTIME_STARTUP_CONTEXT_TOKEN_BUDGET: usize = 5_300;
const REALTIME_ASSISTANT_OUTPUT_TOKEN_BUDGET: usize = 1_000;
const REALTIME_INITIAL_ITEMS_MAX_COUNT: usize = 128;
const REALTIME_INITIAL_ITEMS_MAX_TOKENS: usize = 8_192;
const HANDOFF_STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(200);
const HANDOFF_STREAM_TRUNCATION_MARKER: &str = "\n…output truncated…\n";
const AGENT_FINAL_MESSAGE_PREFIX: &str = "\"Agent Final Message\":\n\n";
const STANDALONE_HANDOFF_ID: &str = "codex";
const DEFAULT_REALTIME_MODEL: &str = "gpt-realtime-1.5";
const DEFAULT_FRAMELESS_REALTIME_MODEL: &str = "gpt-live-1-boulder-alpha";
const REALTIME_USER_TEXT_PREFIX: &str = "[USER] ";
pub const REALTIME_BACKEND_TEXT_PREFIX: &str = "[BACKEND] ";
const REALTIME_V2_HANDOFF_COMPLETE_ACKNOWLEDGEMENT: &str =
    "Background agent finished. Use the preceding [BACKEND] messages as the result.";
const REALTIME_V2_STEER_ACKNOWLEDGEMENT: &str =
    "This was sent to steer the previous background agent task.";
const REALTIME_ACTIVE_RESPONSE_ERROR_PREFIX: &str =
    "Conversation already has an active response in progress:";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RealtimeFanoutTaskStop {
    Await,
    Detach,
}

pub struct RealtimeConversationManager {
    state: Mutex<Option<ConversationState>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RealtimeSessionKind {
    V1,
    V2,
}

#[derive(Clone, Debug)]
struct RealtimeHandoffState {
    output_tx: Sender<RealtimeOutbound>,
    last_output: Arc<Mutex<Option<RealtimeHandoffOutput>>>,
    stream: Arc<Mutex<RealtimeHandoffStreamState>>,
    client_managed_handoffs: bool,
    codex_responses_as_items: bool,
    codex_response_item_prefix: Option<String>,
    codex_response_handoff_mode: CodexResponseHandoffMode,
    session_kind: RealtimeSessionKind,
    event_parser: RealtimeEventParser,
}

#[derive(Clone, Debug)]
struct RealtimeHandoffOutput {
    text: String,
    phase: Option<MessagePhase>,
}

#[derive(Debug, Default)]
struct RealtimeHandoffStreamState {
    active_handoff: Option<String>,
    items: HashMap<String, RealtimeStreamedItem>,
}

#[derive(Debug)]
struct RealtimeStreamedItem {
    handoff_id: String,
    phase: Option<MessagePhase>,
    bem_channel_parser: Option<BemChannelParser>,
    prefix_final_message: bool,
    sent_bytes: usize,
    buffered_text: String,
    tail_text: String,
    truncated: bool,
    last_flush_at: Instant,
    flush_scheduled: bool,
}

impl RealtimeStreamedItem {
    fn next_flush_delay(&self) -> Duration {
        HANDOFF_STREAM_FLUSH_INTERVAL.saturating_sub(self.last_flush_at.elapsed())
    }

    fn output_prefix(&self) -> &'static str {
        if self.prefix_final_message
            && self.sent_bytes == 0
            && !matches!(self.phase, Some(MessagePhase::Commentary))
        {
            AGENT_FINAL_MESSAGE_PREFIX
        } else {
            ""
        }
    }

    fn stream_head_byte_limit(&self) -> usize {
        let output_byte_limit = approx_bytes_for_tokens(REALTIME_ASSISTANT_OUTPUT_TOKEN_BUDGET);
        output_byte_limit.saturating_sub(HANDOFF_STREAM_TRUNCATION_MARKER.len()) / 2
    }

    fn tail_byte_limit(&self) -> usize {
        approx_bytes_for_tokens(REALTIME_ASSISTANT_OUTPUT_TOKEN_BUDGET)
            .saturating_sub(self.stream_head_byte_limit())
            .saturating_sub(HANDOFF_STREAM_TRUNCATION_MARKER.len())
    }

    fn streamable_text_bytes(&self) -> usize {
        self.stream_head_byte_limit()
            .saturating_sub(self.sent_bytes)
            .saturating_sub(self.output_prefix().len())
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let text = if let Some(parser) = self.bem_channel_parser.as_mut() {
            let Some(text) = parser.push(text) else {
                return;
            };
            self.phase = parser.phase();
            text
        } else {
            text.to_string()
        };
        self.push_output_text(&text);
    }

    fn finish_input(&mut self) {
        let Some(parser) = self.bem_channel_parser.as_mut() else {
            return;
        };
        self.phase = parser.phase();
        let text = parser.finish();
        if self.phase.is_none() && !text.is_empty() {
            warn!("BEM output ended before a recognized channel header was received");
            self.phase = Some(MessagePhase::FinalAnswer);
        }
        self.push_output_text(&text);
    }

    fn push_output_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.truncated {
            self.tail_text.push_str(text);
            self.tail_text =
                take_last_bytes_at_char_boundary(&self.tail_text, self.tail_byte_limit())
                    .to_string();
            return;
        }

        self.buffered_text.push_str(text);
        let output_byte_limit = approx_bytes_for_tokens(REALTIME_ASSISTANT_OUTPUT_TOKEN_BUDGET);
        let remaining_text_bytes = output_byte_limit
            .saturating_sub(self.sent_bytes)
            .saturating_sub(self.output_prefix().len());
        if self.buffered_text.len() <= remaining_text_bytes {
            return;
        }

        let head_bytes =
            take_bytes_at_char_boundary(&self.buffered_text, self.streamable_text_bytes()).len();
        self.tail_text =
            take_last_bytes_at_char_boundary(&self.buffered_text, self.tail_byte_limit())
                .to_string();
        self.buffered_text.truncate(head_bytes);
        self.truncated = true;
    }

    fn drain_stream_chunk(&mut self) -> Option<String> {
        let prefix = self.output_prefix();
        let available_text_bytes = self.streamable_text_bytes();
        if self.buffered_text.is_empty() || available_text_bytes == 0 {
            return None;
        }

        let requested_bytes = available_text_bytes.min(self.buffered_text.len());
        let split_at = take_bytes_at_char_boundary(&self.buffered_text, requested_bytes).len();
        if split_at == 0 {
            return None;
        }
        let text = self.buffered_text.drain(..split_at).collect::<String>();
        let text = format!("{prefix}{text}");
        self.sent_bytes += text.len();
        Some(text)
    }

    fn drain_final_chunk(&mut self) -> Option<String> {
        let prefix = self.output_prefix();
        if !self.truncated {
            if self.buffered_text.is_empty() {
                return None;
            }
            let text = self.buffered_text.drain(..).collect::<String>();
            let text = format!("{prefix}{text}");
            self.sent_bytes += text.len();
            return Some(text);
        }

        let head = self.buffered_text.drain(..).collect::<String>();
        let tail = self.tail_text.drain(..).collect::<String>();
        let text = format!("{prefix}{head}{HANDOFF_STREAM_TRUNCATION_MARKER}{tail}");
        self.sent_bytes += text.len();
        Some(text)
    }
}

fn take_last_bytes_at_char_boundary(text: &str, max_bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(max_bytes);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

#[derive(Debug, PartialEq, Eq)]
enum RealtimeOutbound {
    StandaloneHandoff {
        text: String,
        phase: Option<MessagePhase>,
    },
    StandaloneSpeech {
        text: String,
    },
    HandoffUpdate {
        handoff_id: String,
        text: String,
        phase: Option<MessagePhase>,
    },
    HandoffAppend {
        handoff_id: String,
        text: String,
        phase: Option<MessagePhase>,
    },
    CompletedHandoff {
        handoff_id: String,
        text: String,
        phase: Option<MessagePhase>,
    },
    ConversationItem {
        text: String,
        phase: Option<MessagePhase>,
    },
    HandoffCompleteAck {
        handoff_id: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct OutputAudioState {
    item_id: String,
    audio_end_ms: u32,
}

#[derive(Default)]
struct RealtimeResponseCreateQueue {
    active_default_response: bool,
    pending_create: bool,
}

impl RealtimeResponseCreateQueue {
    async fn request_create(
        &mut self,
        writer: &RealtimeWebsocketWriter,
        events_tx: &Sender<RealtimeEvent>,
        reason: &str,
    ) -> anyhow::Result<()> {
        if self.active_default_response {
            self.pending_create = true;
            return Ok(());
        }
        self.send_create_now(writer, events_tx, reason).await
    }

    fn mark_started(&mut self) {
        self.active_default_response = true;
    }

    async fn mark_finished(
        &mut self,
        writer: &RealtimeWebsocketWriter,
        events_tx: &Sender<RealtimeEvent>,
        reason: &str,
    ) -> anyhow::Result<()> {
        self.active_default_response = false;
        if !self.pending_create {
            return Ok(());
        }
        self.pending_create = false;
        self.send_create_now(writer, events_tx, reason).await
    }

    async fn send_create_now(
        &mut self,
        writer: &RealtimeWebsocketWriter,
        events_tx: &Sender<RealtimeEvent>,
        reason: &str,
    ) -> anyhow::Result<()> {
        if let Err(err) = writer.send_response_create().await {
            let mapped_error = map_api_error(err);
            let error_message = mapped_error.to_string();
            if error_message.starts_with(REALTIME_ACTIVE_RESPONSE_ERROR_PREFIX) {
                warn!("realtime response.create raced an active response; deferring");
                self.active_default_response = true;
                self.pending_create = true;
                return Ok(());
            }
            warn!("failed to send {reason} response.create: {mapped_error}");
            let _ = events_tx.send(RealtimeEvent::Error(error_message)).await;
            return Err(mapped_error.into());
        }
        self.active_default_response = true;
        Ok(())
    }
}

struct RealtimeInputTask {
    writer: RealtimeWebsocketWriter,
    events: RealtimeWebsocketEvents,
    text_rx: Receiver<ConversationTextParams>,
    handoff_output_rx: Receiver<RealtimeOutbound>,
    audio_rx: Receiver<RealtimeAudioFrame>,
    events_tx: Sender<RealtimeEvent>,
    handoff_state: RealtimeHandoffState,
    session_kind: RealtimeSessionKind,
    event_parser: RealtimeEventParser,
    flush_transcript_tail_on_session_end: bool,
    transcript_tail_tx: Sender<String>,
    stop_token: CancellationToken,
}

struct RealtimeInputChannels {
    text_rx: Receiver<ConversationTextParams>,
    handoff_output_rx: Receiver<RealtimeOutbound>,
    audio_rx: Receiver<RealtimeAudioFrame>,
}

impl RealtimeHandoffState {
    fn new(
        output_tx: Sender<RealtimeOutbound>,
        client_managed_handoffs: bool,
        codex_responses_as_items: bool,
        codex_response_item_prefix: Option<String>,
        codex_response_handoff_mode: CodexResponseHandoffMode,
        session_kind: RealtimeSessionKind,
        event_parser: RealtimeEventParser,
    ) -> Self {
        Self {
            output_tx,
            last_output: Arc::new(Mutex::new(None)),
            stream: Arc::new(Mutex::new(RealtimeHandoffStreamState::default())),
            client_managed_handoffs,
            codex_responses_as_items,
            codex_response_item_prefix,
            codex_response_handoff_mode,
            session_kind,
            event_parser,
        }
    }

    fn streams_handoff_append(&self) -> bool {
        self.event_parser == RealtimeEventParser::FramelessBidi
            && !self.client_managed_handoffs
            && !self.codex_responses_as_items
    }

    fn routes_handoff_by_bem(&self) -> bool {
        self.event_parser == RealtimeEventParser::FramelessBidi
            && self.codex_response_handoff_mode == CodexResponseHandoffMode::BemTags
    }
}

#[allow(dead_code)]
struct ConversationState {
    audio_tx: Sender<RealtimeAudioFrame>,
    text_tx: Sender<ConversationTextParams>,
    session_kind: RealtimeSessionKind,
    handoff: RealtimeHandoffState,
    input_task: JoinHandle<()>,
    fanout_task: Option<JoinHandle<()>>,
    realtime_active: Arc<AtomicBool>,
    stop_token: CancellationToken,
}

/// All transport inputs needed to establish a realtime conversation.
pub struct RealtimeStart {
    pub api_provider: ApiProvider,
    pub extra_headers: Option<HeaderMap>,
    pub client_managed_handoffs: bool,
    pub flush_transcript_tail_on_session_end: bool,
    pub codex_responses_as_items: bool,
    pub codex_response_item_prefix: Option<String>,
    pub codex_response_handoff_mode: CodexResponseHandoffMode,
    pub realtime_call_api_provider: Option<ApiProvider>,
    pub session_config: RealtimeSessionConfig,
    pub model_client: ModelClient,
    pub sdp: Option<String>,
}

/// Channels and media data returned after a realtime conversation starts.
pub struct RealtimeStartOutput {
    pub realtime_active: Arc<AtomicBool>,
    pub events_rx: Receiver<RealtimeEvent>,
    /// Emits the unwrapped transcript tail when the realtime transport closes.
    pub transcript_tail_rx: Receiver<String>,
    pub sdp: Option<String>,
}

#[allow(dead_code)]
impl RealtimeConversationManager {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(None),
        }
    }

    pub async fn running_state(&self) -> Option<()> {
        let state = self.state.lock().await;
        state
            .as_ref()
            .and_then(|state| state.realtime_active.load(Ordering::Relaxed).then_some(()))
    }

    pub async fn is_running_v2(&self) -> bool {
        let state = self.state.lock().await;
        matches!(
            state.as_ref(),
            Some(state)
                if state.realtime_active.load(Ordering::Relaxed)
                    && state.session_kind == RealtimeSessionKind::V2
        )
    }

    pub async fn start(&self, start: RealtimeStart) -> CodexResult<RealtimeStartOutput> {
        let previous_state = {
            let mut guard = self.state.lock().await;
            guard.take()
        };
        if let Some(state) = previous_state {
            stop_conversation_state(state, RealtimeFanoutTaskStop::Await).await;
        }

        self.start_inner(start).await
    }

    async fn start_inner(&self, start: RealtimeStart) -> CodexResult<RealtimeStartOutput> {
        let RealtimeStart {
            api_provider,
            extra_headers,
            client_managed_handoffs,
            flush_transcript_tail_on_session_end,
            codex_responses_as_items,
            codex_response_item_prefix,
            codex_response_handoff_mode,
            realtime_call_api_provider,
            session_config,
            model_client,
            sdp,
        } = start;
        let event_parser = session_config.event_parser;
        let session_kind = match event_parser {
            RealtimeEventParser::V1 | RealtimeEventParser::FramelessBidi => RealtimeSessionKind::V1,
            RealtimeEventParser::RealtimeV2 => RealtimeSessionKind::V2,
        };

        let (audio_tx, audio_rx) =
            async_channel::bounded::<RealtimeAudioFrame>(AUDIO_IN_QUEUE_CAPACITY);
        let (text_tx, text_rx) =
            async_channel::bounded::<ConversationTextParams>(TEXT_IN_QUEUE_CAPACITY);
        let (handoff_output_tx, handoff_output_rx) =
            async_channel::bounded::<RealtimeOutbound>(HANDOFF_OUT_QUEUE_CAPACITY);
        let (events_tx, events_rx) =
            async_channel::bounded::<RealtimeEvent>(OUTPUT_EVENTS_QUEUE_CAPACITY);
        let (transcript_tail_tx, transcript_tail_rx) = async_channel::bounded::<String>(1);

        let realtime_active = Arc::new(AtomicBool::new(true));
        let stop_token = CancellationToken::new();
        let handoff = RealtimeHandoffState::new(
            handoff_output_tx,
            client_managed_handoffs,
            codex_responses_as_items,
            codex_response_item_prefix,
            codex_response_handoff_mode,
            session_kind,
            event_parser,
        );
        let input_channels = RealtimeInputChannels {
            text_rx,
            handoff_output_rx,
            audio_rx,
        };

        let client = RealtimeWebsocketClient::new(api_provider);
        let (task, sdp) = if let Some(sdp) = sdp {
            let call = model_client
                .create_realtime_call_with_headers(
                    sdp,
                    session_config.clone(),
                    extra_headers.unwrap_or_default(),
                    realtime_call_api_provider,
                )
                .await?;
            let task = spawn_webrtc_sideband_input_task(RealtimeWebrtcSidebandInputTask {
                client,
                session_config,
                call_id: call.call_id,
                sideband_headers: call.sideband_headers,
                input_channels,
                events_tx,
                handoff_state: handoff.clone(),
                session_kind,
                event_parser,
                realtime_active: Arc::clone(&realtime_active),
                flush_transcript_tail_on_session_end,
                transcript_tail_tx,
                stop_token: stop_token.clone(),
            });
            (task, Some(call.sdp))
        } else {
            let connection = client
                .connect(
                    session_config,
                    extra_headers.unwrap_or_default(),
                    default_headers(),
                )
                .await
                .map_err(map_api_error)?;
            let task = spawn_realtime_input_task(RealtimeInputTask {
                writer: connection.writer(),
                events: connection.events(),
                text_rx: input_channels.text_rx,
                handoff_output_rx: input_channels.handoff_output_rx,
                audio_rx: input_channels.audio_rx,
                events_tx,
                handoff_state: handoff.clone(),
                session_kind,
                event_parser,
                flush_transcript_tail_on_session_end,
                transcript_tail_tx,
                stop_token: stop_token.clone(),
            });
            (task, None)
        };

        let mut guard = self.state.lock().await;
        *guard = Some(ConversationState {
            audio_tx,
            text_tx,
            session_kind,
            handoff,
            input_task: task,
            fanout_task: None,
            realtime_active: Arc::clone(&realtime_active),
            stop_token,
        });
        Ok(RealtimeStartOutput {
            realtime_active,
            events_rx,
            transcript_tail_rx,
            sdp,
        })
    }

    pub async fn register_fanout_task(
        &self,
        realtime_active: &Arc<AtomicBool>,
        fanout_task: JoinHandle<()>,
    ) {
        let mut fanout_task = Some(fanout_task);
        {
            let mut guard = self.state.lock().await;
            if let Some(state) = guard.as_mut()
                && Arc::ptr_eq(&state.realtime_active, realtime_active)
            {
                state.fanout_task = fanout_task.take();
            }
        }

        if let Some(fanout_task) = fanout_task {
            fanout_task.abort();
            let _ = fanout_task.await;
        }
    }

    pub async fn finish_if_active(&self, realtime_active: &Arc<AtomicBool>) {
        let state = {
            let mut guard = self.state.lock().await;
            match guard.as_ref() {
                Some(state) if Arc::ptr_eq(&state.realtime_active, realtime_active) => guard.take(),
                _ => None,
            }
        };

        if let Some(state) = state {
            stop_conversation_state(state, RealtimeFanoutTaskStop::Detach).await;
        }
    }

    pub async fn audio_in(&self, frame: RealtimeAudioFrame) -> CodexResult<()> {
        let sender = {
            let guard = self.state.lock().await;
            guard.as_ref().map(|state| state.audio_tx.clone())
        };

        let Some(sender) = sender else {
            return Err(CodexErr::InvalidRequest(
                "conversation is not running".to_string(),
            ));
        };

        match sender.try_send(frame) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                warn!("dropping input audio frame due to full queue");
                Ok(())
            }
            Err(TrySendError::Closed(_)) => Err(CodexErr::InvalidRequest(
                "conversation is not running".to_string(),
            )),
        }
    }

    pub async fn text_in(&self, mut params: ConversationTextParams) -> CodexResult<()> {
        let sender = {
            let guard = self.state.lock().await;
            guard
                .as_ref()
                .map(|state| (state.text_tx.clone(), state.session_kind))
        };

        let Some((sender, session_kind)) = sender else {
            return Err(CodexErr::InvalidRequest(
                "conversation is not running".to_string(),
            ));
        };

        if params.role == ConversationTextRole::User {
            params.text =
                prefix_realtime_text(params.text, REALTIME_USER_TEXT_PREFIX, session_kind);
        }
        sender
            .send(params)
            .await
            .map_err(|_| CodexErr::InvalidRequest("conversation is not running".to_string()))?;
        Ok(())
    }

    pub async fn handoff_out(
        &self,
        output_text: String,
        phase: Option<MessagePhase>,
    ) -> CodexResult<()> {
        let handoff = {
            let guard = self.state.lock().await;
            let Some(state) = guard.as_ref() else {
                return Err(CodexErr::InvalidRequest(
                    "conversation is not running".to_string(),
                ));
            };
            state.handoff.clone()
        };

        if handoff.client_managed_handoffs {
            return Ok(());
        }
        let phase = if handoff.routes_handoff_by_bem() {
            match bem_message_phase(&output_text) {
                Some(phase) => Some(phase),
                None => {
                    warn!("BEM output did not contain a recognized channel header");
                    Some(MessagePhase::FinalAnswer)
                }
            }
        } else {
            phase
        };
        let is_commentary = matches!(phase, Some(MessagePhase::Commentary));
        let active_handoff = handoff.stream.lock().await.active_handoff.clone();
        let output = match active_handoff {
            Some(handoff_id) => {
                let output_text = realtime_backend_output(output_text, handoff.session_kind);
                *handoff.last_output.lock().await = Some(RealtimeHandoffOutput {
                    text: output_text.clone(),
                    phase: phase.clone(),
                });
                if handoff.codex_responses_as_items {
                    RealtimeOutbound::ConversationItem {
                        text: realtime_backend_item(
                            output_text,
                            handoff.codex_response_item_prefix.as_deref(),
                        ),
                        phase,
                    }
                } else if handoff.event_parser == RealtimeEventParser::V1 && is_commentary {
                    RealtimeOutbound::HandoffAppend {
                        handoff_id,
                        text: output_text,
                        phase,
                    }
                } else {
                    RealtimeOutbound::HandoffUpdate {
                        handoff_id,
                        text: output_text,
                        phase,
                    }
                }
            }
            None if output_text.trim().is_empty() => return Ok(()),
            None => {
                let output_text = realtime_backend_output(output_text, handoff.session_kind);
                if handoff.codex_responses_as_items {
                    RealtimeOutbound::ConversationItem {
                        text: realtime_backend_item(
                            output_text,
                            handoff.codex_response_item_prefix.as_deref(),
                        ),
                        phase,
                    }
                } else {
                    RealtimeOutbound::StandaloneHandoff {
                        text: if handoff.event_parser == RealtimeEventParser::V1 && !is_commentary {
                            format!("{AGENT_FINAL_MESSAGE_PREFIX}{output_text}")
                        } else {
                            output_text
                        },
                        phase,
                    }
                }
            }
        };
        handoff
            .output_tx
            .send(output)
            .await
            .map_err(|_| CodexErr::InvalidRequest("conversation is not running".to_string()))?;
        Ok(())
    }

    pub async fn register_handoff_stream_item(
        &self,
        item_id: String,
        phase: Option<MessagePhase>,
        initial_text: String,
    ) {
        let handoff = {
            let guard = self.state.lock().await;
            guard.as_ref().map(|state| state.handoff.clone())
        };
        let Some(handoff) = handoff else {
            return;
        };
        if !handoff.streams_handoff_append() {
            return;
        }
        let flush_delay = {
            let mut stream = handoff.stream.lock().await;
            let Some(handoff_id) = stream.active_handoff.clone() else {
                return;
            };
            let mut streamed_item = RealtimeStreamedItem {
                handoff_id,
                phase: if handoff.routes_handoff_by_bem() {
                    None
                } else {
                    phase
                },
                bem_channel_parser: handoff
                    .routes_handoff_by_bem()
                    .then(BemChannelParser::default),
                prefix_final_message: handoff.event_parser == RealtimeEventParser::V1,
                sent_bytes: 0,
                buffered_text: String::new(),
                tail_text: String::new(),
                truncated: false,
                last_flush_at: Instant::now(),
                flush_scheduled: false,
            };
            streamed_item.push_text(&initial_text);
            let flush_delay = if streamed_item.buffered_text.is_empty() {
                None
            } else {
                streamed_item.flush_scheduled = true;
                Some(streamed_item.next_flush_delay())
            };
            stream.items.insert(item_id.clone(), streamed_item);
            flush_delay
        };
        if let Some(flush_delay) = flush_delay {
            schedule_streamed_handoff_flush(&handoff, item_id, flush_delay);
        }
    }

    pub async fn stream_handoff_delta(&self, item_id: &str, delta: String) -> CodexResult<()> {
        if delta.is_empty() {
            return Ok(());
        }
        let handoff = {
            let guard = self.state.lock().await;
            let Some(state) = guard.as_ref() else {
                return Err(CodexErr::InvalidRequest(
                    "conversation is not running".to_string(),
                ));
            };
            state.handoff.clone()
        };
        if !handoff.streams_handoff_append() {
            return Ok(());
        }
        let flush_delay = {
            let mut stream = handoff.stream.lock().await;
            let Some(streamed_item) = stream.items.get_mut(item_id) else {
                return Ok(());
            };
            streamed_item.push_text(&delta);
            if streamed_item.flush_scheduled || streamed_item.streamable_text_bytes() == 0 {
                None
            } else {
                streamed_item.flush_scheduled = true;
                Some(streamed_item.next_flush_delay())
            }
        };
        if let Some(flush_delay) = flush_delay {
            schedule_streamed_handoff_flush(&handoff, item_id.to_string(), flush_delay);
        }
        Ok(())
    }

    pub async fn finish_handoff_stream_item(&self, item_id: &str) -> bool {
        let handoff = {
            let guard = self.state.lock().await;
            guard.as_ref().map(|state| state.handoff.clone())
        };
        let Some(handoff) = handoff else {
            return false;
        };
        if !handoff.streams_handoff_append() {
            return false;
        }
        let Some(mut streamed_item) = handoff.stream.lock().await.items.remove(item_id) else {
            return false;
        };
        streamed_item.finish_input();
        let chunk = streamed_item.drain_final_chunk();
        let sent_output = streamed_item.sent_bytes > 0;
        if let Some(text) = chunk {
            let _ = handoff
                .output_tx
                .send(RealtimeOutbound::HandoffAppend {
                    handoff_id: streamed_item.handoff_id,
                    text,
                    phase: streamed_item.phase,
                })
                .await;
        }
        sent_output
    }

    pub async fn append_speech(&self, text: String) -> CodexResult<()> {
        if text.trim().is_empty() {
            return Ok(());
        }

        let handoff = {
            let guard = self.state.lock().await;
            let Some(state) = guard.as_ref() else {
                return Err(CodexErr::InvalidRequest(
                    "conversation is not running".to_string(),
                ));
            };
            state.handoff.clone()
        };

        handoff
            .output_tx
            .send(RealtimeOutbound::StandaloneSpeech {
                text: realtime_backend_output(text, handoff.session_kind),
            })
            .await
            .map_err(|_| CodexErr::InvalidRequest("conversation is not running".to_string()))?;
        Ok(())
    }

    pub async fn handoff_complete(&self) -> CodexResult<()> {
        let handoff = {
            let guard = self.state.lock().await;
            guard.as_ref().map(|state| state.handoff.clone())
        };
        let Some(handoff) = handoff else {
            return Ok(());
        };
        if handoff.client_managed_handoffs {
            return Ok(());
        }
        match handoff.session_kind {
            RealtimeSessionKind::V1 => return Ok(()),
            RealtimeSessionKind::V2 => {}
        }

        let Some(handoff_id) = handoff.stream.lock().await.active_handoff.clone() else {
            return Ok(());
        };
        let Some(last_output) = handoff.last_output.lock().await.clone() else {
            return Ok(());
        };

        let output = if handoff.codex_responses_as_items {
            RealtimeOutbound::HandoffCompleteAck { handoff_id }
        } else {
            RealtimeOutbound::CompletedHandoff {
                handoff_id,
                text: last_output.text,
                phase: last_output.phase,
            }
        };

        handoff
            .output_tx
            .send(output)
            .await
            .map_err(|_| CodexErr::InvalidRequest("conversation is not running".to_string()))
    }

    pub async fn clear_active_handoff(&self) {
        let handoff = {
            let guard = self.state.lock().await;
            guard.as_ref().map(|state| state.handoff.clone())
        };
        if let Some(handoff) = handoff {
            {
                let mut stream = handoff.stream.lock().await;
                stream.active_handoff = None;
                stream.items.clear();
            }
            *handoff.last_output.lock().await = None;
        }
    }

    pub async fn shutdown(&self) -> CodexResult<()> {
        let state = {
            let mut guard = self.state.lock().await;
            guard.take()
        };

        if let Some(state) = state {
            stop_conversation_state(state, RealtimeFanoutTaskStop::Await).await;
        }
        Ok(())
    }
}

async fn stop_conversation_state(
    mut state: ConversationState,
    fanout_task_stop: RealtimeFanoutTaskStop,
) {
    state.realtime_active.store(false, Ordering::Relaxed);
    state.stop_token.cancel();
    let _ = state.input_task.await;

    if let Some(fanout_task) = state.fanout_task.take() {
        match fanout_task_stop {
            RealtimeFanoutTaskStop::Await => {
                let _ = fanout_task.await;
            }
            RealtimeFanoutTaskStop::Detach => {}
        }
    }
}

fn prefix_realtime_text(text: String, prefix: &str, session_kind: RealtimeSessionKind) -> String {
    if session_kind != RealtimeSessionKind::V2 || text.is_empty() || text.starts_with(prefix) {
        return text;
    }
    format!("{prefix}{text}")
}

fn realtime_backend_output(output_text: String, session_kind: RealtimeSessionKind) -> String {
    let output_text = prefix_realtime_text(output_text, REALTIME_BACKEND_TEXT_PREFIX, session_kind);
    truncate_realtime_text_to_token_budget(&output_text, REALTIME_ASSISTANT_OUTPUT_TOKEN_BUDGET)
}

fn realtime_backend_item(text: String, prefix: Option<&str>) -> String {
    let text = match prefix.filter(|prefix| !prefix.is_empty()) {
        Some(prefix) => format!("{prefix}\n\n{text}"),
        None => text,
    };
    truncate_realtime_text_to_token_budget(&text, REALTIME_ASSISTANT_OUTPUT_TOKEN_BUDGET)
}

fn realtime_transcript_delta(active_transcript: &[RealtimeTranscriptEntry]) -> Option<String> {
    let active_transcript = active_transcript
        .iter()
        .map(|entry| format!("{role}: {text}", role = entry.role, text = entry.text))
        .collect::<Vec<_>>()
        .join("\n");
    (!active_transcript.is_empty()).then_some(active_transcript)
}

fn spawn_realtime_input_task(input: RealtimeInputTask) -> JoinHandle<()> {
    tokio::spawn(run_realtime_input_task(input))
}

struct RealtimeWebrtcSidebandInputTask {
    client: RealtimeWebsocketClient,
    session_config: RealtimeSessionConfig,
    call_id: String,
    sideband_headers: HeaderMap,
    input_channels: RealtimeInputChannels,
    events_tx: Sender<RealtimeEvent>,
    handoff_state: RealtimeHandoffState,
    session_kind: RealtimeSessionKind,
    event_parser: RealtimeEventParser,
    realtime_active: Arc<AtomicBool>,
    flush_transcript_tail_on_session_end: bool,
    transcript_tail_tx: Sender<String>,
    stop_token: CancellationToken,
}

fn spawn_webrtc_sideband_input_task(input: RealtimeWebrtcSidebandInputTask) -> JoinHandle<()> {
    let RealtimeWebrtcSidebandInputTask {
        client,
        session_config,
        call_id,
        sideband_headers,
        input_channels,
        events_tx,
        handoff_state,
        session_kind,
        event_parser,
        realtime_active,
        flush_transcript_tail_on_session_end,
        transcript_tail_tx,
        stop_token,
    } = input;

    tokio::spawn(async move {
        if !realtime_active.load(Ordering::Relaxed) {
            return;
        }

        let connection = match tokio::select! {
            connection = client.connect_webrtc_sideband(
                session_config,
                &call_id,
                sideband_headers,
                default_headers(),
            ) => connection,
            _ = stop_token.cancelled() => return,
        } {
            Ok(connection) => connection,
            Err(err) => {
                if realtime_active.load(Ordering::Relaxed) {
                    let mapped_error = map_api_error(err);
                    warn!("failed to connect realtime sideband: {mapped_error}");
                    let _ = events_tx
                        .send(RealtimeEvent::Error(mapped_error.to_string()))
                        .await;
                }
                return;
            }
        };

        if !realtime_active.load(Ordering::Relaxed) {
            return;
        }

        run_realtime_input_task(RealtimeInputTask {
            writer: connection.writer(),
            events: connection.events(),
            text_rx: input_channels.text_rx,
            handoff_output_rx: input_channels.handoff_output_rx,
            audio_rx: input_channels.audio_rx,
            events_tx,
            handoff_state,
            session_kind,
            event_parser,
            flush_transcript_tail_on_session_end,
            transcript_tail_tx,
            stop_token,
        })
        .await;
    })
}

async fn run_realtime_input_task(input: RealtimeInputTask) {
    let RealtimeInputTask {
        writer,
        events,
        text_rx,
        handoff_output_rx,
        audio_rx,
        events_tx,
        handoff_state,
        session_kind,
        event_parser,
        flush_transcript_tail_on_session_end,
        transcript_tail_tx,
        stop_token,
    } = input;

    let mut output_audio_state: Option<OutputAudioState> = None;
    let mut response_create_queue = RealtimeResponseCreateQueue::default();

    loop {
        let result = tokio::select! {
            _ = stop_token.cancelled() => break,
            // Text input that should be sent into realtime.
            text = text_rx.recv() => {
                handle_text_input(
                    text,
                    &writer,
                    &events_tx,
                )
                    .await
            }
            // Background agent progress or final output that should be sent back to realtime.
            background_agent_output = handoff_output_rx.recv() => {
                handle_handoff_output(
                    background_agent_output,
                    &writer,
                    &events_tx,
                    &handoff_state,
                    event_parser,
                    &mut response_create_queue,
                )
                    .await
            }
            // Events received from the realtime server.
            realtime_event = events.next_event() => {
                handle_realtime_server_event(
                    realtime_event,
                    &writer,
                    &events_tx,
                    &handoff_state,
                    session_kind,
                    &mut output_audio_state,
                    &mut response_create_queue,
                )
                .await
            }
            // Audio frames captured from the user microphone.
            user_audio_frame = audio_rx.recv() => {
                handle_user_audio_input(user_audio_frame, &writer, &events_tx)
                    .await
            }
        };
        if result.is_err() {
            break;
        }
    }

    if flush_transcript_tail_on_session_end
        && let Some(transcript_delta) =
            realtime_transcript_delta(&events.take_transcript_tail().await)
    {
        let _ = transcript_tail_tx.send(transcript_delta).await;
    }
}

async fn handle_text_input(
    params: Result<ConversationTextParams, RecvError>,
    writer: &RealtimeWebsocketWriter,
    events_tx: &Sender<RealtimeEvent>,
) -> anyhow::Result<()> {
    let params = params.context("text input channel closed")?;

    if let Err(err) = writer
        .send_conversation_item_create(params.text, params.role)
        .await
    {
        let mapped_error = map_api_error(err);
        warn!("failed to send input text: {mapped_error}");
        let _ = events_tx
            .send(RealtimeEvent::Error(mapped_error.to_string()))
            .await;
        return Err(mapped_error.into());
    }
    Ok(())
}

async fn flush_streamed_handoff_item(handoff: &RealtimeHandoffState, item_id: &str) {
    let (handoff_id, text, phase) = {
        let mut stream = handoff.stream.lock().await;
        let Some(streamed_item) = stream.items.get_mut(item_id) else {
            return;
        };
        streamed_item.flush_scheduled = false;
        let Some(text) = streamed_item.drain_stream_chunk() else {
            return;
        };
        streamed_item.last_flush_at = Instant::now();
        (
            streamed_item.handoff_id.clone(),
            text,
            streamed_item.phase.clone(),
        )
    };
    let _ = handoff
        .output_tx
        .send(RealtimeOutbound::HandoffAppend {
            handoff_id,
            text,
            phase,
        })
        .await;
}

fn schedule_streamed_handoff_flush(
    handoff: &RealtimeHandoffState,
    item_id: String,
    flush_delay: Duration,
) {
    let handoff = handoff.clone();
    let _flush_task = tokio::spawn(async move {
        tokio::time::sleep(flush_delay).await;
        flush_streamed_handoff_item(&handoff, &item_id).await;
    });
}

fn v3_output_writer(
    writer: &RealtimeWebsocketWriter,
    phase: Option<&MessagePhase>,
    handoff_mode: CodexResponseHandoffMode,
) -> RealtimeWebsocketWriter {
    let channel = match handoff_mode {
        CodexResponseHandoffMode::Thinking => None,
        CodexResponseHandoffMode::Commentary => Some(RealtimeContextAppendChannel::Commentary),
        CodexResponseHandoffMode::BemTags => match phase {
            Some(MessagePhase::FinalAnswer) => Some(RealtimeContextAppendChannel::Speakable),
            Some(MessagePhase::Commentary) => Some(RealtimeContextAppendChannel::Commentary),
            None => Some(RealtimeContextAppendChannel::Speakable),
        },
    };
    match channel {
        Some(channel) => writer.clone().with_context_append_channel(channel),
        None => writer.clone(),
    }
}

async fn handle_handoff_output(
    handoff_output: Result<RealtimeOutbound, RecvError>,
    writer: &RealtimeWebsocketWriter,
    events_tx: &Sender<RealtimeEvent>,
    handoff_state: &RealtimeHandoffState,
    event_parser: RealtimeEventParser,
    response_create_queue: &mut RealtimeResponseCreateQueue,
) -> anyhow::Result<()> {
    let handoff_output = handoff_output.context("handoff output channel closed")?;
    let result = match event_parser {
        RealtimeEventParser::V1 => match handoff_output {
            RealtimeOutbound::StandaloneHandoff { text, phase: _ } => {
                writer
                    .send_standalone_handoff(STANDALONE_HANDOFF_ID.to_string(), text)
                    .await
            }
            RealtimeOutbound::StandaloneSpeech { text } => {
                writer
                    .send_standalone_handoff(STANDALONE_HANDOFF_ID.to_string(), text)
                    .await
            }
            RealtimeOutbound::HandoffUpdate {
                handoff_id,
                text,
                phase: _,
            }
            | RealtimeOutbound::CompletedHandoff {
                handoff_id,
                text,
                phase: _,
            } => {
                writer
                    .send_conversation_function_call_output(handoff_id, text)
                    .await
            }
            RealtimeOutbound::HandoffAppend {
                handoff_id,
                text,
                phase: _,
            } => {
                writer
                    .send_conversation_handoff_append(handoff_id, text)
                    .await
            }
            RealtimeOutbound::ConversationItem { text, phase: _ } => {
                writer
                    .send_conversation_item_create(text, ConversationTextRole::Developer)
                    .await
            }
            RealtimeOutbound::HandoffCompleteAck { .. } => Ok(()),
        },
        RealtimeEventParser::FramelessBidi => match handoff_output {
            RealtimeOutbound::StandaloneHandoff { text, phase } => {
                v3_output_writer(
                    writer,
                    phase.as_ref(),
                    handoff_state.codex_response_handoff_mode,
                )
                .send_standalone_handoff(STANDALONE_HANDOFF_ID.to_string(), text)
                .await
            }
            RealtimeOutbound::StandaloneSpeech { text } => {
                writer
                    .clone()
                    .with_context_append_channel(RealtimeContextAppendChannel::Speakable)
                    .send_standalone_handoff(STANDALONE_HANDOFF_ID.to_string(), text)
                    .await
            }
            RealtimeOutbound::HandoffUpdate {
                handoff_id,
                text,
                phase,
            } => {
                v3_output_writer(
                    writer,
                    phase.as_ref(),
                    handoff_state.codex_response_handoff_mode,
                )
                .send_conversation_function_call_output(handoff_id, text)
                .await
            }
            RealtimeOutbound::HandoffAppend {
                handoff_id,
                text,
                phase,
            } => {
                v3_output_writer(
                    writer,
                    phase.as_ref(),
                    handoff_state.codex_response_handoff_mode,
                )
                .send_conversation_handoff_append(handoff_id, text)
                .await
            }
            RealtimeOutbound::CompletedHandoff {
                handoff_id,
                text,
                phase,
            } => {
                v3_output_writer(
                    writer,
                    phase.as_ref(),
                    handoff_state.codex_response_handoff_mode,
                )
                .send_conversation_function_call_output(handoff_id, text)
                .await
            }
            RealtimeOutbound::ConversationItem { text, phase } => {
                v3_output_writer(
                    writer,
                    phase.as_ref(),
                    handoff_state.codex_response_handoff_mode,
                )
                .send_conversation_item_create(text, ConversationTextRole::Developer)
                .await
            }
            RealtimeOutbound::HandoffCompleteAck { .. } => Ok(()),
        },
        RealtimeEventParser::RealtimeV2 => match handoff_output {
            RealtimeOutbound::StandaloneHandoff { text, phase: _ } => {
                if let Err(err) = writer
                    .send_conversation_item_create(text, ConversationTextRole::User)
                    .await
                {
                    Err(err)
                } else {
                    return response_create_queue
                        .request_create(writer, events_tx, "standalone handoff")
                        .await;
                }
            }
            RealtimeOutbound::StandaloneSpeech { text } => {
                if let Err(err) = writer
                    .send_conversation_item_create(text, ConversationTextRole::User)
                    .await
                {
                    Err(err)
                } else {
                    return response_create_queue
                        .request_create(writer, events_tx, "standalone handoff")
                        .await;
                }
            }
            RealtimeOutbound::HandoffUpdate {
                handoff_id,
                text,
                phase: _,
            }
            | RealtimeOutbound::HandoffAppend {
                handoff_id,
                text,
                phase: _,
            } => {
                let active_handoff = handoff_state.stream.lock().await.active_handoff.clone();
                match active_handoff {
                    Some(active_handoff) if active_handoff == handoff_id => {}
                    Some(_) | None => {
                        debug!("dropping stale realtime handoff progress update");
                        return Ok(());
                    }
                }
                writer
                    .send_conversation_item_create(text, ConversationTextRole::User)
                    .await
            }
            RealtimeOutbound::CompletedHandoff {
                handoff_id,
                text: _,
                phase: _,
            } => {
                if let Err(err) = writer
                    .send_conversation_function_call_output(
                        handoff_id,
                        REALTIME_V2_HANDOFF_COMPLETE_ACKNOWLEDGEMENT.to_string(),
                    )
                    .await
                {
                    Err(err)
                } else {
                    return response_create_queue
                        .request_create(writer, events_tx, "handoff")
                        .await;
                }
            }
            RealtimeOutbound::ConversationItem { text, phase: _ } => {
                writer
                    .send_conversation_item_create(text, ConversationTextRole::Developer)
                    .await
            }
            RealtimeOutbound::HandoffCompleteAck { handoff_id } => {
                writer
                    .send_conversation_function_call_output(handoff_id, String::new())
                    .await
            }
        },
    };
    if let Err(err) = result {
        let mapped_error = map_api_error(err);
        warn!("failed to send handoff output: {mapped_error}");
        let _ = events_tx
            .send(RealtimeEvent::Error(mapped_error.to_string()))
            .await;
        return Err(mapped_error.into());
    }
    Ok(())
}

async fn handle_realtime_server_event(
    event: Result<Option<RealtimeEvent>, ApiError>,
    writer: &RealtimeWebsocketWriter,
    events_tx: &Sender<RealtimeEvent>,
    handoff_state: &RealtimeHandoffState,
    session_kind: RealtimeSessionKind,
    output_audio_state: &mut Option<OutputAudioState>,
    response_create_queue: &mut RealtimeResponseCreateQueue,
) -> anyhow::Result<()> {
    let event = match event {
        Ok(Some(event)) => event,
        Ok(None) => anyhow::bail!("realtime event stream ended"),
        Err(err) => {
            let mapped_error = map_api_error(err);
            if events_tx
                .send(RealtimeEvent::Error(mapped_error.to_string()))
                .await
                .is_err()
            {
                return Err(mapped_error.into());
            }
            error!("realtime stream closed: {mapped_error}");
            return Err(mapped_error.into());
        }
    };

    let should_stop = match &event {
        RealtimeEvent::AudioOut(frame) => {
            match session_kind {
                RealtimeSessionKind::V1 => {}
                RealtimeSessionKind::V2 => {
                    update_output_audio_state(output_audio_state, frame);
                }
            }
            false
        }
        RealtimeEvent::InputAudioSpeechStarted(event) => {
            match session_kind {
                RealtimeSessionKind::V1 => {}
                RealtimeSessionKind::V2 => {
                    if let Some(output_audio_state) = output_audio_state.take()
                        && event
                            .item_id
                            .as_deref()
                            .is_none_or(|item_id| item_id == output_audio_state.item_id)
                        && let Err(err) = writer
                            .send_payload(
                                json!({
                                    "type": "conversation.item.truncate",
                                    "item_id": output_audio_state.item_id,
                                    "content_index": 0,
                                    "audio_end_ms": output_audio_state.audio_end_ms,
                                })
                                .to_string(),
                            )
                            .await
                    {
                        let mapped_error = map_api_error(err);
                        warn!("failed to truncate realtime audio: {mapped_error}");
                    }
                }
            }
            false
        }
        RealtimeEvent::ResponseCreated(_) => {
            match session_kind {
                RealtimeSessionKind::V1 => {}
                RealtimeSessionKind::V2 => response_create_queue.mark_started(),
            }
            false
        }
        RealtimeEvent::ResponseCancelled(_) => {
            *output_audio_state = None;
            match session_kind {
                RealtimeSessionKind::V1 => {}
                RealtimeSessionKind::V2 => {
                    response_create_queue
                        .mark_finished(writer, events_tx, "deferred")
                        .await?;
                }
            }
            false
        }
        RealtimeEvent::ResponseDone(_) => {
            *output_audio_state = None;
            match session_kind {
                RealtimeSessionKind::V1 => {}
                RealtimeSessionKind::V2 => {
                    response_create_queue
                        .mark_finished(writer, events_tx, "deferred")
                        .await?;
                }
            }
            false
        }
        RealtimeEvent::HandoffRequested(handoff) => {
            *output_audio_state = None;

            match session_kind {
                RealtimeSessionKind::V1 => {
                    let mut stream = handoff_state.stream.lock().await;
                    stream.items.clear();
                    stream.active_handoff = Some(handoff.handoff_id.clone());
                }
                RealtimeSessionKind::V2 => {
                    let active_handoff = handoff_state.stream.lock().await.active_handoff.clone();
                    match active_handoff {
                        Some(_) => {
                            if let Err(err) = writer
                                .send_conversation_function_call_output(
                                    handoff.handoff_id.clone(),
                                    REALTIME_V2_STEER_ACKNOWLEDGEMENT.to_string(),
                                )
                                .await
                            {
                                let mapped_error = map_api_error(err);
                                warn!(
                                    "failed to send handoff steering acknowledgement: {mapped_error}"
                                );
                                let _ = events_tx
                                    .send(RealtimeEvent::Error(mapped_error.to_string()))
                                    .await;
                                return Err(mapped_error.into());
                            }
                            response_create_queue
                                .request_create(writer, events_tx, "handoff steering")
                                .await?;
                        }
                        None => {
                            handoff_state.stream.lock().await.active_handoff =
                                Some(handoff.handoff_id.clone());
                        }
                    }
                }
            }
            false
        }
        RealtimeEvent::NoopRequested(noop) => {
            *output_audio_state = None;

            match session_kind {
                RealtimeSessionKind::V1 => {}
                RealtimeSessionKind::V2 => {
                    if let Err(err) = writer
                        .send_conversation_function_call_output(noop.call_id.clone(), String::new())
                        .await
                    {
                        let mapped_error = map_api_error(err);
                        warn!("failed to send realtime noop function output: {mapped_error}");
                        let _ = events_tx
                            .send(RealtimeEvent::Error(mapped_error.to_string()))
                            .await;
                        return Err(mapped_error.into());
                    }
                }
            }
            false
        }
        RealtimeEvent::Error(_) => true,
        RealtimeEvent::SessionUpdated {
            realtime_session_id,
            ..
        } => {
            info!(realtime_session_id = %realtime_session_id, "realtime session updated");
            false
        }
        RealtimeEvent::InputTranscriptDelta(_)
        | RealtimeEvent::InputTranscriptDone(_)
        | RealtimeEvent::OutputTranscriptDelta(_)
        | RealtimeEvent::OutputTranscriptDone(_)
        | RealtimeEvent::ConversationItemAdded(_)
        | RealtimeEvent::ConversationItemDone { .. } => false,
    };

    if events_tx.send(event).await.is_err() {
        anyhow::bail!("realtime output event channel closed");
    }
    if should_stop {
        error!("realtime stream error event received");
        anyhow::bail!("realtime stream error event received");
    }
    Ok(())
}

async fn handle_user_audio_input(
    frame: Result<RealtimeAudioFrame, RecvError>,
    writer: &RealtimeWebsocketWriter,
    events_tx: &Sender<RealtimeEvent>,
) -> anyhow::Result<()> {
    let frame = frame.context("user audio input channel closed")?;

    if let Err(err) = writer.send_audio_frame(frame).await {
        let mapped_error = map_api_error(err);
        error!("failed to send input audio: {mapped_error}");
        let _ = events_tx
            .send(RealtimeEvent::Error(mapped_error.to_string()))
            .await;
        return Err(mapped_error.into());
    }
    Ok(())
}

fn update_output_audio_state(
    output_audio_state: &mut Option<OutputAudioState>,
    frame: &RealtimeAudioFrame,
) {
    let Some(item_id) = frame.item_id.clone() else {
        return;
    };
    let audio_end_ms = audio_duration_ms(frame);
    if audio_end_ms == 0 {
        return;
    }

    if let Some(current) = output_audio_state.as_mut()
        && current.item_id == item_id
    {
        current.audio_end_ms = current.audio_end_ms.saturating_add(audio_end_ms);
        return;
    }

    *output_audio_state = Some(OutputAudioState {
        item_id,
        audio_end_ms,
    });
}

fn audio_duration_ms(frame: &RealtimeAudioFrame) -> u32 {
    let Some(samples_per_channel) = frame
        .samples_per_channel
        .or(decoded_samples_per_channel(frame))
    else {
        return 0;
    };
    let sample_rate = u64::from(frame.sample_rate.max(1));
    ((u64::from(samples_per_channel) * 1_000) / sample_rate) as u32
}

fn decoded_samples_per_channel(frame: &RealtimeAudioFrame) -> Option<u32> {
    let bytes = BASE64_STANDARD.decode(&frame.data).ok()?;
    let channels = usize::from(frame.num_channels.max(1));
    let samples = bytes.len().checked_div(2)?.checked_div(channels)?;
    u32::try_from(samples).ok()
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
