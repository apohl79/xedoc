const PERSONAL_ACCESS_TOKEN_PREFIX: &str = "at-";

pub(super) enum XedocAccessToken<'a> {
    PersonalAccessToken(&'a str),
    AgentIdentityJwt(&'a str),
}

pub(super) fn classify_xedoc_access_token(access_token: &str) -> XedocAccessToken<'_> {
    if access_token.starts_with(PERSONAL_ACCESS_TOKEN_PREFIX) {
        XedocAccessToken::PersonalAccessToken(access_token)
    } else {
        XedocAccessToken::AgentIdentityJwt(access_token)
    }
}

#[cfg(test)]
#[path = "access_token_tests.rs"]
mod tests;
