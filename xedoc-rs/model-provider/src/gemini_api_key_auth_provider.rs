use http::HeaderMap;
use http::HeaderValue;
use xedoc_api::AuthProvider;

#[derive(Clone)]
pub(crate) struct GeminiApiKeyAuthProvider {
    api_key: String,
}

impl GeminiApiKeyAuthProvider {
    pub(crate) fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

impl AuthProvider for GeminiApiKeyAuthProvider {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        if let Ok(header) = HeaderValue::from_str(&self.api_key) {
            let _ = headers.insert("x-goog-api-key", header);
        }
    }
}
