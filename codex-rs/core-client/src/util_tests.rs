use super::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use tracing::Event;
use tracing::Subscriber;
use tracing::field::Visit;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Default)]
struct TagCollectorVisitor {
    tags: BTreeMap<String, String>,
}

impl Visit for TagCollectorVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.tags
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

#[derive(Clone)]
struct TagCollectorLayer {
    tags: Arc<Mutex<BTreeMap<String, String>>>,
    event_count: Arc<Mutex<usize>>,
}

impl<S> Layer<S> for TagCollectorLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != "feedback_tags" {
            return;
        }
        let mut visitor = TagCollectorVisitor::default();
        event.record(&mut visitor);
        self.tags.lock().unwrap().extend(visitor.tags);
        *self.event_count.lock().unwrap() += 1;
    }
}

#[test]
fn emit_feedback_auth_recovery_tags_preserves_401_specific_fields() {
    let tags = Arc::new(Mutex::new(BTreeMap::new()));
    let event_count = Arc::new(Mutex::new(0));
    let _guard = tracing_subscriber::registry()
        .with(TagCollectorLayer {
            tags: tags.clone(),
            event_count: event_count.clone(),
        })
        .set_default();

    emit_feedback_auth_recovery_tags(
        "managed",
        "refresh_token",
        "recovery_succeeded",
        Some("req-401"),
        Some("ray-401"),
        Some("missing_authorization_header"),
        Some("token_expired"),
    );

    let tags = tags.lock().unwrap().clone();
    assert_eq!(
        tags.get("auth_401_request_id").map(String::as_str),
        Some("\"req-401\"")
    );
    assert_eq!(
        tags.get("auth_401_cf_ray").map(String::as_str),
        Some("\"ray-401\"")
    );
    assert_eq!(
        tags.get("auth_401_error").map(String::as_str),
        Some("\"missing_authorization_header\"")
    );
    assert_eq!(
        tags.get("auth_401_error_code").map(String::as_str),
        Some("\"token_expired\"")
    );
    assert_eq!(*event_count.lock().unwrap(), 1);
}

#[test]
fn emit_feedback_auth_recovery_tags_clears_stale_401_fields() {
    let tags = Arc::new(Mutex::new(BTreeMap::new()));
    let event_count = Arc::new(Mutex::new(0));
    let _guard = tracing_subscriber::registry()
        .with(TagCollectorLayer {
            tags: tags.clone(),
            event_count: event_count.clone(),
        })
        .set_default();

    emit_feedback_auth_recovery_tags(
        "managed",
        "refresh_token",
        "recovery_failed_transient",
        Some("req-401-a"),
        Some("ray-401-a"),
        Some("missing_authorization_header"),
        Some("token_expired"),
    );
    emit_feedback_auth_recovery_tags(
        "managed",
        "done",
        "recovery_not_run",
        Some("req-401-b"),
        /*auth_cf_ray*/ None,
        /*auth_error*/ None,
        /*auth_error_code*/ None,
    );

    let tags = tags.lock().unwrap().clone();
    assert_eq!(
        tags.get("auth_401_request_id").map(String::as_str),
        Some("\"req-401-b\"")
    );
    assert_eq!(
        tags.get("auth_401_cf_ray").map(String::as_str),
        Some("\"\"")
    );
    assert_eq!(tags.get("auth_401_error").map(String::as_str), Some("\"\""));
    assert_eq!(
        tags.get("auth_401_error_code").map(String::as_str),
        Some("\"\"")
    );
    assert_eq!(*event_count.lock().unwrap(), 2);
}
