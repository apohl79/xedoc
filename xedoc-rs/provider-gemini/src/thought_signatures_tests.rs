use pretty_assertions::assert_eq;

use super::GeminiThoughtSignatureStore;
use super::MAX_THOUGHT_SIGNATURES;

#[test]
fn ignores_empty_keys_and_signatures() {
    let store = GeminiThoughtSignatureStore::new();
    store.remember("", "signature");
    store.remember("call", "");

    assert_eq!((store.signature(""), store.signature("call")), (None, None));
}

#[test]
fn retains_entries_through_the_capacity_boundary() {
    let store = GeminiThoughtSignatureStore::new();
    (0..MAX_THOUGHT_SIGNATURES).for_each(|index| {
        store.remember(&format!("call-{index}"), &format!("signature-{index}"));
    });

    assert_eq!(
        (
            store.signature("call-0"),
            store.signature(&format!("call-{}", MAX_THOUGHT_SIGNATURES - 1)),
        ),
        (
            Some("signature-0".to_string()),
            Some(format!("signature-{}", MAX_THOUGHT_SIGNATURES - 1)),
        )
    );
}

#[test]
fn evicts_the_oldest_key_after_capacity() {
    let store = GeminiThoughtSignatureStore::new();
    (0..=MAX_THOUGHT_SIGNATURES).for_each(|index| {
        store.remember(&format!("call-{index}"), &format!("signature-{index}"));
    });

    assert_eq!(
        (
            store.signature("call-0"),
            store.signature("call-1"),
            store.signature(&format!("call-{MAX_THOUGHT_SIGNATURES}")),
        ),
        (
            None,
            Some("signature-1".to_string()),
            Some(format!("signature-{MAX_THOUGHT_SIGNATURES}")),
        )
    );
}

#[test]
fn replacing_an_entry_does_not_consume_capacity() {
    let store = GeminiThoughtSignatureStore::new();
    store.remember("existing", "old");
    store.remember("existing", "new");
    (1..MAX_THOUGHT_SIGNATURES).for_each(|index| {
        store.remember(&format!("call-{index}"), &format!("signature-{index}"));
    });

    assert_eq!(store.signature("existing"), Some("new".to_string()));
}
