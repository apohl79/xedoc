use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::context::ContextualUserFragment;
use crate::context::RealtimeDelegation;
use crate::context::RealtimeDelegationSource;
use crate::realtime_context::build_realtime_startup_context;
use crate::realtime_prompt::prepare_realtime_backend_prompt;
use crate::session::session::Session;
use codex_api::Provider as ApiProvider;
use codex_api::RealtimeEvent;
use codex_api::RealtimeEventParser;
use codex_api::RealtimeSessionConfig;
use codex_api::RealtimeSessionMode;
use codex_config::config_toml::RealtimeWsMode;
use codex_config::config_toml::RealtimeWsVersion;
pub(crate) use codex_core_realtime::RealtimeConversationManager;
use codex_core_realtime::RealtimeStart;
use codex_core_realtime::RealtimeStartOutput;
use codex_login::CodexAuth;
use codex_login::default_client::add_originator_header;
use codex_login::read_openai_api_key_from_env;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::auth::AuthMode;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::CodexResponseHandoffMode;
use codex_protocol::protocol::ConversationAudioParams;
use codex_protocol::protocol::ConversationSpeechParams;
use codex_protocol::protocol::ConversationStartParams;
use codex_protocol::protocol::ConversationStartTransport;
use codex_protocol::protocol::ConversationTextParams;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RealtimeConversationClosedEvent;
use codex_protocol::protocol::RealtimeConversationRealtimeEvent;
use codex_protocol::protocol::RealtimeConversationSdpEvent;
use codex_protocol::protocol::RealtimeConversationStartedEvent;
use codex_protocol::protocol::RealtimeHandoffRequested;
use codex_protocol::protocol::RealtimeOutputModality;
use codex_protocol::protocol::RealtimeTranscriptEntry;
use codex_protocol::protocol::RealtimeVoice;
use codex_protocol::protocol::RealtimeVoicesList;
use codex_utils_string::approx_token_count;
use http::HeaderMap;
use http::HeaderValue;
use http::header::AUTHORIZATION;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

const REALTIME_STARTUP_CONTEXT_TOKEN_BUDGET: usize = 5_300;
const REALTIME_INITIAL_ITEMS_MAX_COUNT: usize = 128;
const REALTIME_INITIAL_ITEMS_MAX_TOKENS: usize = 8_192;
const DEFAULT_REALTIME_MODEL: &str = "gpt-realtime-1.5";
const DEFAULT_FRAMELESS_REALTIME_MODEL: &str = "gpt-live-1-boulder-alpha";
const REALTIME_SESSION_ENDED_HANDOFF_INSTRUCTION: &str = "The user just ended their realtime session. Here is the remaining handoff/transcript tail. You probably do not have to do anything; acknowledge the handoff unless the transcript itself asks for something.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RealtimeConversationEnd {
    Requested,
    TransportClosed,
    Error,
}

pub(crate) async fn handle_start(
    sess: &Arc<Session>,
    sub_id: String,
    params: ConversationStartParams,
) -> CodexResult<()> {
    let prepared_start = match prepare_realtime_start(sess, params).await {
        Ok(prepared_start) => prepared_start,
        Err(err) => {
            error!("failed to prepare realtime conversation: {err}");
            let message = err.to_string();
            sess.send_event_raw(Event {
                id: sub_id,
                msg: EventMsg::RealtimeConversationRealtime(RealtimeConversationRealtimeEvent {
                    payload: RealtimeEvent::Error(message),
                }),
            })
            .await;
            return Ok(());
        }
    };

    if let Err(err) = handle_start_inner(sess, &sub_id, prepared_start).await {
        error!("failed to start realtime conversation: {err}");
        let message = err.to_string();
        sess.send_event_raw(Event {
            id: sub_id.clone(),
            msg: EventMsg::RealtimeConversationRealtime(RealtimeConversationRealtimeEvent {
                payload: RealtimeEvent::Error(message),
            }),
        })
        .await;
    }
    Ok(())
}

struct PreparedRealtimeConversationStart {
    api_provider: ApiProvider,
    extra_headers: Option<HeaderMap>,
    client_managed_handoffs: bool,
    flush_transcript_tail_on_session_end: bool,
    codex_responses_as_items: bool,
    codex_response_item_prefix: Option<String>,
    codex_response_handoff_mode: CodexResponseHandoffMode,
    realtime_call_api_provider: Option<ApiProvider>,
    requested_realtime_session_id: Option<String>,
    version: RealtimeWsVersion,
    session_config: RealtimeSessionConfig,
    transport: ConversationStartTransport,
}

#[derive(Clone, Copy)]
pub(crate) enum ConfiguredRealtimeVoice {
    Use,
    Ignore,
}

async fn prepare_realtime_start(
    sess: &Arc<Session>,
    params: ConversationStartParams,
) -> CodexResult<PreparedRealtimeConversationStart> {
    let provider = sess.provider().await;
    let auth_manager = sess
        .services
        .model_client
        .load()
        .auth_manager()
        .unwrap_or_else(|| Arc::clone(&sess.services.auth_manager));
    let auth = auth_manager.auth().await;
    let config = sess.get_config().await;
    let transport = params
        .transport
        .clone()
        .unwrap_or(ConversationStartTransport::Websocket);
    let mut api_provider = provider.to_api_provider(Some(AuthMode::ApiKey))?;
    if let Some(realtime_ws_base_url) = &config.experimental_realtime_ws_base_url {
        api_provider.base_url = realtime_ws_base_url.clone();
    }
    let realtime_call_api_provider =
        if let Some(realtime_call_base_url) = &config.experimental_realtime_webrtc_call_base_url {
            let mut api_provider = provider.to_api_provider(Some(AuthMode::ApiKey))?;
            api_provider.base_url = realtime_call_base_url.clone();
            Some(api_provider)
        } else {
            None
        };
    let version = params.version.unwrap_or(match &transport {
        ConversationStartTransport::Websocket => config.realtime.version,
        ConversationStartTransport::Webrtc { .. } => RealtimeWsVersion::V1,
    });
    if matches!(transport, ConversationStartTransport::Webrtc { .. }) {
        validate_avas_webrtc_start(version, config.realtime.session_type)?;
    }
    let configured_voice = match (&transport, params.version) {
        (ConversationStartTransport::Webrtc { .. }, None) => ConfiguredRealtimeVoice::Ignore,
        (ConversationStartTransport::Webrtc { .. } | ConversationStartTransport::Websocket, _) => {
            ConfiguredRealtimeVoice::Use
        }
    };
    let session_config =
        build_realtime_session_config(sess, &params, version, configured_voice).await?;
    let requested_realtime_session_id = session_config.session_id.clone();
    let event_parser = session_config.event_parser;
    let originator = sess.originator().await;
    let extra_headers = match transport {
        ConversationStartTransport::Websocket => {
            let realtime_api_key = realtime_api_key(auth.as_ref(), &provider)?;
            realtime_request_headers(
                requested_realtime_session_id.as_deref(),
                Some(realtime_api_key.as_str()),
                event_parser,
                originator.as_str(),
            )?
        }
        ConversationStartTransport::Webrtc { .. } => {
            realtime_request_headers(
                requested_realtime_session_id.as_deref(),
                /*api_key*/ None,
                event_parser,
                originator.as_str(),
            )?
        }
    };
    Ok(PreparedRealtimeConversationStart {
        api_provider,
        extra_headers,
        client_managed_handoffs: params.client_managed_handoffs,
        flush_transcript_tail_on_session_end: params.flush_transcript_tail_on_session_end,
        codex_responses_as_items: params.codex_responses_as_items,
        codex_response_item_prefix: params.codex_response_item_prefix,
        codex_response_handoff_mode: params.codex_response_handoff_mode,
        realtime_call_api_provider,
        requested_realtime_session_id,
        version,
        session_config,
        transport,
    })
}

fn validate_avas_webrtc_start(
    version: RealtimeWsVersion,
    session_type: RealtimeWsMode,
) -> CodexResult<()> {
    if version == RealtimeWsVersion::V2 {
        return Err(CodexErr::InvalidRequest(
            "AVAS realtime calls require realtime v1 or v3".to_string(),
        ));
    }
    if session_type != RealtimeWsMode::Conversational {
        return Err(CodexErr::InvalidRequest(
            "AVAS realtime calls require conversational realtime".to_string(),
        ));
    }
    Ok(())
}

pub(crate) async fn build_realtime_session_config(
    sess: &Arc<Session>,
    params: &ConversationStartParams,
    version: RealtimeWsVersion,
    configured_voice: ConfiguredRealtimeVoice,
) -> CodexResult<RealtimeSessionConfig> {
    let config = sess.get_config().await;
    let prompt = prepare_realtime_backend_prompt(
        params.prompt.clone(),
        config.experimental_realtime_ws_backend_prompt.clone(),
    );
    let startup_context = if params.include_startup_context {
        match config.experimental_realtime_ws_startup_context.clone() {
            Some(startup_context) => startup_context,
            None => {
                build_realtime_startup_context(sess.as_ref(), REALTIME_STARTUP_CONTEXT_TOKEN_BUDGET)
                    .await
                    .unwrap_or_default()
            }
        }
    } else {
        String::new()
    };
    let prompt = match (prompt.is_empty(), startup_context.is_empty()) {
        (true, true) => String::new(),
        (true, false) => startup_context,
        (false, true) => prompt,
        (false, false) => format!("{prompt}\n\n{startup_context}"),
    };
    if version != RealtimeWsVersion::V3 && !params.initial_items.is_empty() {
        return Err(CodexErr::InvalidRequest(
            "initial realtime items require realtime v3".to_string(),
        ));
    }
    if params.initial_items.len() > REALTIME_INITIAL_ITEMS_MAX_COUNT {
        return Err(CodexErr::InvalidRequest(format!(
            "initial realtime items must contain no more than {REALTIME_INITIAL_ITEMS_MAX_COUNT} items"
        )));
    }
    let mut total_initial_item_tokens: usize = 0;
    for item in &params.initial_items {
        let item_tokens = approx_token_count(&item.text);
        if item_tokens > REALTIME_INITIAL_ITEMS_MAX_TOKENS {
            return Err(CodexErr::InvalidRequest(format!(
                "each initial realtime item must not exceed {REALTIME_INITIAL_ITEMS_MAX_TOKENS} estimated tokens"
            )));
        }
        total_initial_item_tokens = total_initial_item_tokens.saturating_add(item_tokens);
    }
    if total_initial_item_tokens > REALTIME_INITIAL_ITEMS_MAX_TOKENS {
        return Err(CodexErr::InvalidRequest(format!(
            "initial realtime items must not exceed {REALTIME_INITIAL_ITEMS_MAX_TOKENS} estimated tokens in total"
        )));
    }
    let model = Some(
        params
            .model
            .clone()
            .or_else(|| config.experimental_realtime_ws_model.clone())
            .unwrap_or_else(|| match version {
                RealtimeWsVersion::V1 | RealtimeWsVersion::V2 => DEFAULT_REALTIME_MODEL.to_string(),
                RealtimeWsVersion::V3 => DEFAULT_FRAMELESS_REALTIME_MODEL.to_string(),
            }),
    );
    let event_parser = match version {
        RealtimeWsVersion::V1 => RealtimeEventParser::V1,
        RealtimeWsVersion::V2 => RealtimeEventParser::RealtimeV2,
        RealtimeWsVersion::V3 => RealtimeEventParser::FramelessBidi,
    };
    if version != RealtimeWsVersion::V2
        && matches!(params.output_modality, RealtimeOutputModality::Text)
    {
        return Err(CodexErr::InvalidRequest(
            "text realtime output modality requires realtime v2".to_string(),
        ));
    }
    let session_mode = match config.realtime.session_type {
        RealtimeWsMode::Conversational => RealtimeSessionMode::Conversational,
        RealtimeWsMode::Transcription => RealtimeSessionMode::Transcription,
    };
    let config_voice = match configured_voice {
        ConfiguredRealtimeVoice::Use => config.realtime.voice,
        ConfiguredRealtimeVoice::Ignore => None,
    };
    let voice = params
        .voice
        .or(config_voice)
        .unwrap_or_else(|| default_realtime_voice(version));
    validate_realtime_voice(version, voice)?;
    Ok(RealtimeSessionConfig {
        instructions: prompt,
        initial_items: params.initial_items.clone(),
        model,
        session_id: Some(
            params
                .realtime_session_id
                .clone()
                .unwrap_or_else(|| sess.thread_id.to_string()),
        ),
        event_parser,
        session_mode,
        output_modality: params.output_modality,
        voice,
    })
}

fn default_realtime_voice(version: RealtimeWsVersion) -> RealtimeVoice {
    let voices = RealtimeVoicesList::builtin();
    match version {
        RealtimeWsVersion::V1 | RealtimeWsVersion::V3 => voices.default_v1,
        RealtimeWsVersion::V2 => voices.default_v2,
    }
}

fn validate_realtime_voice(version: RealtimeWsVersion, voice: RealtimeVoice) -> CodexResult<()> {
    let voices = RealtimeVoicesList::builtin();
    let allowed = match version {
        RealtimeWsVersion::V1 | RealtimeWsVersion::V3 => &voices.v1,
        RealtimeWsVersion::V2 => &voices.v2,
    };
    if allowed.contains(&voice) {
        return Ok(());
    }

    let version = match version {
        RealtimeWsVersion::V1 => "v1",
        RealtimeWsVersion::V2 => "v2",
        RealtimeWsVersion::V3 => "v3",
    };
    let allowed = allowed
        .iter()
        .map(|voice| voice.wire_name())
        .collect::<Vec<_>>()
        .join(", ");
    Err(CodexErr::InvalidRequest(format!(
        "realtime voice `{}` is not supported for {version}; supported voices: {allowed}",
        voice.wire_name()
    )))
}

async fn handle_start_inner(
    sess: &Arc<Session>,
    sub_id: &str,
    prepared_start: PreparedRealtimeConversationStart,
) -> CodexResult<()> {
    let PreparedRealtimeConversationStart {
        api_provider,
        extra_headers,
        client_managed_handoffs,
        flush_transcript_tail_on_session_end,
        codex_responses_as_items,
        codex_response_item_prefix,
        codex_response_handoff_mode,
        realtime_call_api_provider,
        requested_realtime_session_id,
        version,
        session_config,
        transport,
    } = prepared_start;
    info!("starting realtime conversation");
    let sdp = match transport {
        ConversationStartTransport::Websocket => None,
        ConversationStartTransport::Webrtc { sdp } => Some(sdp),
    };
    let start = RealtimeStart {
        api_provider,
        extra_headers,
        client_managed_handoffs,
        flush_transcript_tail_on_session_end,
        codex_responses_as_items,
        codex_response_item_prefix,
        codex_response_handoff_mode,
        realtime_call_api_provider,
        session_config,
        model_client: sess.services.model_client.load_full().as_ref().clone(),
        sdp,
    };
    let start_output = sess.conversation.start(start).await?;

    info!("realtime conversation started");

    sess.send_event_raw(Event {
        id: sub_id.to_string(),
        msg: EventMsg::RealtimeConversationStarted(RealtimeConversationStartedEvent {
            realtime_session_id: requested_realtime_session_id,
            version,
        }),
    })
    .await;

    let RealtimeStartOutput {
        realtime_active,
        events_rx,
        transcript_tail_rx,
        sdp,
    } = start_output;
    if let Some(sdp) = sdp {
        sess.send_event_raw(Event {
            id: sub_id.to_string(),
            msg: EventMsg::RealtimeConversationSdp(RealtimeConversationSdpEvent { sdp }),
        })
        .await;
    }

    let sess_clone = Arc::clone(sess);
    let sub_id = sub_id.to_string();
    let fanout_realtime_active = Arc::clone(&realtime_active);
    let fanout_task = tokio::spawn(async move {
        let ev = |msg| Event {
            id: sub_id.clone(),
            msg,
        };
        let mut end = RealtimeConversationEnd::TransportClosed;
        // Drain already-parsed events so a queued handoff is routed before the final tail.
        while let Ok(event) = events_rx.recv().await {
            match &event {
                RealtimeEvent::AudioOut(_) => {}
                _ => {
                    info!(
                        event = ?event,
                        "received realtime conversation event"
                    );
                }
            }
            if let RealtimeEvent::Error(_) = &event {
                end = RealtimeConversationEnd::Error;
            }
            let maybe_routed_text = match &event {
                RealtimeEvent::HandoffRequested(handoff) => {
                    realtime_delegation_from_handoff(handoff)
                }
                _ => None,
            };
            if let Some(text) = maybe_routed_text {
                debug!(text = %text, "[realtime-text] realtime conversation text output");
                let sess_for_routed_text = Arc::clone(&sess_clone);
                sess_for_routed_text.route_realtime_text_input(text).await;
            }
            sess_clone
                .send_event_raw(ev(EventMsg::RealtimeConversationRealtime(
                    RealtimeConversationRealtimeEvent {
                        payload: event.clone(),
                    },
                )))
                .await;
        }
        if let Ok(transcript_delta) = transcript_tail_rx.recv().await {
            let text = wrap_realtime_delegation_input(
                REALTIME_SESSION_ENDED_HANDOFF_INSTRUCTION,
                Some(&transcript_delta),
                RealtimeDelegationSource::TranscriptTailFlush,
            );
            sess_clone.route_realtime_text_input(text).await;
        }
        if fanout_realtime_active.swap(false, Ordering::Relaxed) {
            match end {
                RealtimeConversationEnd::TransportClosed => {
                    info!("realtime conversation transport closed");
                }
                RealtimeConversationEnd::Requested | RealtimeConversationEnd::Error => {}
            }
            sess_clone
                .conversation
                .finish_if_active(&fanout_realtime_active)
                .await;
            send_realtime_conversation_closed(&sess_clone, sub_id, end).await;
        }
    });
    sess.conversation
        .register_fanout_task(&realtime_active, fanout_task)
        .await;

    Ok(())
}

pub(crate) async fn handle_audio(
    sess: &Arc<Session>,
    sub_id: String,
    params: ConversationAudioParams,
) {
    if let Err(err) = sess.conversation.audio_in(params.frame).await {
        error!("failed to append realtime audio: {err}");
        if sess.conversation.running_state().await.is_some() {
            warn!("realtime audio input failed while the session was already ending");
        } else {
            send_conversation_error(sess, sub_id, err.to_string(), CodexErrorInfo::BadRequest)
                .await;
        }
    }
}

fn realtime_transcript_delta_from_handoff(handoff: &RealtimeHandoffRequested) -> Option<String> {
    realtime_transcript_delta(&handoff.active_transcript)
}

fn realtime_transcript_delta(active_transcript: &[RealtimeTranscriptEntry]) -> Option<String> {
    let active_transcript = active_transcript
        .iter()
        .map(|entry| format!("{role}: {text}", role = entry.role, text = entry.text))
        .collect::<Vec<_>>()
        .join("\n");
    (!active_transcript.is_empty()).then_some(active_transcript)
}

fn realtime_text_from_handoff_request(handoff: &RealtimeHandoffRequested) -> Option<String> {
    (!handoff.input_transcript.is_empty())
        .then_some(handoff.input_transcript.clone())
        .or_else(|| realtime_transcript_delta_from_handoff(handoff))
}

fn realtime_delegation_from_handoff(handoff: &RealtimeHandoffRequested) -> Option<String> {
    let input = realtime_text_from_handoff_request(handoff)?;
    Some(wrap_realtime_delegation_input(
        &input,
        realtime_transcript_delta_from_handoff(handoff).as_deref(),
        RealtimeDelegationSource::Handoff,
    ))
}

fn wrap_realtime_delegation_input(
    input: &str,
    transcript_delta: Option<&str>,
    source: RealtimeDelegationSource,
) -> String {
    RealtimeDelegation::new(input, transcript_delta, source).render()
}

fn realtime_api_key(auth: Option<&CodexAuth>, provider: &ModelProviderInfo) -> CodexResult<String> {
    if let Some(api_key) = provider.api_key()? {
        return Ok(api_key);
    }

    if let Some(token) = provider.experimental_bearer_token.clone() {
        return Ok(token);
    }

    if let Some(api_key) = auth.and_then(CodexAuth::api_key) {
        return Ok(api_key.to_string());
    }

    // TODO(aibrahim): Remove this temporary fallback once realtime auth no longer
    // requires API key auth for ChatGPT/SIWC sessions.
    if provider.is_openai()
        && let Some(api_key) = read_openai_api_key_from_env()
    {
        return Ok(api_key);
    }

    Err(CodexErr::InvalidRequest(
        "realtime conversation requires API key auth".to_string(),
    ))
}

fn realtime_request_headers(
    realtime_session_id: Option<&str>,
    api_key: Option<&str>,
    event_parser: RealtimeEventParser,
    originator: &str,
) -> CodexResult<Option<HeaderMap>> {
    let mut headers = HeaderMap::new();

    match event_parser {
        RealtimeEventParser::V1 => {
            headers.insert("openai-alpha", HeaderValue::from_static("quicksilver=v1"));
        }
        RealtimeEventParser::FramelessBidi => {
            headers.insert("openai-alpha", HeaderValue::from_static("quicksilver=v2"));
        }
        RealtimeEventParser::RealtimeV2 => {}
    }

    if let Some(realtime_session_id) = realtime_session_id
        && let Ok(realtime_session_id) = HeaderValue::from_str(realtime_session_id)
    {
        headers.insert("x-session-id", realtime_session_id);
    }

    if let Some(api_key) = api_key {
        let auth_value = HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|err| {
            CodexErr::InvalidRequest(format!("invalid realtime api key header: {err}"))
        })?;
        headers.insert(AUTHORIZATION, auth_value);
    }

    add_originator_header(&mut headers, originator);

    Ok(Some(headers))
}

pub(crate) async fn handle_text(
    sess: &Arc<Session>,
    sub_id: String,
    params: ConversationTextParams,
) {
    debug!(text = %params.text, "[realtime-text] appending realtime conversation text input");
    if let Err(err) = sess.conversation.text_in(params).await {
        error!("failed to append realtime text: {err}");
        if sess.conversation.running_state().await.is_some() {
            warn!("realtime text input failed while the session was already ending");
        } else {
            send_conversation_error(sess, sub_id, err.to_string(), CodexErrorInfo::BadRequest)
                .await;
        }
    }
}

pub(crate) async fn handle_speech(
    sess: &Arc<Session>,
    sub_id: String,
    params: ConversationSpeechParams,
) {
    debug!(text = %params.text, "[realtime-text] appending realtime speech");
    if let Err(err) = sess.conversation.append_speech(params.text).await {
        error!("failed to append realtime speech: {err}");
        if sess.conversation.running_state().await.is_some() {
            warn!("realtime speech append failed while the session was already ending");
        } else {
            send_conversation_error(sess, sub_id, err.to_string(), CodexErrorInfo::BadRequest)
                .await;
        }
    }
}

pub(crate) async fn handle_close(sess: &Arc<Session>, sub_id: String) {
    end_realtime_conversation(sess, sub_id, RealtimeConversationEnd::Requested).await;
}

async fn send_conversation_error(
    sess: &Arc<Session>,
    sub_id: String,
    message: String,
    codex_error_info: CodexErrorInfo,
) {
    sess.send_event_raw(Event {
        id: sub_id,
        msg: EventMsg::Error(ErrorEvent {
            message,
            codex_error_info: Some(codex_error_info),
        }),
    })
    .await;
}

async fn end_realtime_conversation(
    sess: &Arc<Session>,
    sub_id: String,
    end: RealtimeConversationEnd,
) {
    let _ = sess.conversation.shutdown().await;
    send_realtime_conversation_closed(sess, sub_id, end).await;
}

async fn send_realtime_conversation_closed(
    sess: &Arc<Session>,
    sub_id: String,
    end: RealtimeConversationEnd,
) {
    let reason = match end {
        RealtimeConversationEnd::Requested => Some("requested".to_string()),
        RealtimeConversationEnd::TransportClosed => Some("transport_closed".to_string()),
        RealtimeConversationEnd::Error => Some("error".to_string()),
    };

    sess.send_event_raw(Event {
        id: sub_id,
        msg: EventMsg::RealtimeConversationClosed(RealtimeConversationClosedEvent { reason }),
    })
    .await;
}

#[cfg(test)]
#[path = "realtime_conversation_tests.rs"]
mod tests;
