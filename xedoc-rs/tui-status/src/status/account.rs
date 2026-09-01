#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusAccountDisplay {
    ChatGpt {
        email: Option<String>,
        plan: Option<String>,
    },
    ApiKey,
}
