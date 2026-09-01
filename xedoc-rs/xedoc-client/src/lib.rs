mod retry;
mod sse;
mod telemetry;

pub use crate::retry::RetryOn;
pub use crate::retry::RetryPolicy;
pub use crate::retry::backoff;
pub use crate::retry::run_with_retry;
pub use crate::sse::sse_stream;
pub use crate::telemetry::RequestTelemetry;
pub use xedoc_http_client::HttpClient as XedocHttpClient;
pub use xedoc_http_client::RequestBuilder as XedocRequestBuilder;
pub use xedoc_http_client::*;
