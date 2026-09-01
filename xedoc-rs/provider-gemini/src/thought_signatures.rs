use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt;
use std::sync::Mutex;

const MAX_THOUGHT_SIGNATURES: usize = 4096;

#[derive(Default)]
pub struct GeminiThoughtSignatureStore {
    entries: Mutex<ThoughtSignatures>,
}

#[derive(Default)]
struct ThoughtSignatures {
    insertion_order: VecDeque<String>,
    by_call_id: HashMap<String, String>,
}

impl fmt::Debug for GeminiThoughtSignatureStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GeminiThoughtSignatureStore")
            .finish_non_exhaustive()
    }
}

impl GeminiThoughtSignatureStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn remember(&self, call_id: &str, signature: &str) {
        if call_id.is_empty() || signature.is_empty() {
            return;
        }
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !entries.by_call_id.contains_key(call_id) {
            entries.insertion_order.push_back(call_id.to_string());
        }
        entries
            .by_call_id
            .insert(call_id.to_string(), signature.to_string());
        if entries.by_call_id.len() > MAX_THOUGHT_SIGNATURES
            && let Some(oldest) = entries.insertion_order.pop_front()
        {
            entries.by_call_id.remove(&oldest);
        }
    }

    pub(crate) fn signature(&self, call_id: &str) -> Option<String> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_call_id
            .get(call_id)
            .cloned()
    }
}

#[cfg(test)]
#[path = "thought_signatures_tests.rs"]
mod tests;
