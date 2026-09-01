use http::HeaderMap;
use http::HeaderValue;
use xedoc_api::AuthProvider;

#[derive(Clone)]
pub(crate) struct AnthropicApiKeyAuthProvider {
    api_key: String,
}

impl AnthropicApiKeyAuthProvider {
    pub(crate) fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

impl AuthProvider for AnthropicApiKeyAuthProvider {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        if let Ok(header) = HeaderValue::from_str(&self.api_key) {
            let _ = headers.insert("x-api-key", header);
        }
    }
}
