use super::*;

#[test]
fn classifies_personal_access_tokens_by_prefix() {
    assert!(matches!(
        classify_xedoc_access_token("at-example"),
        XedocAccessToken::PersonalAccessToken("at-example")
    ));
    assert!(matches!(
        classify_xedoc_access_token("header.payload.signature"),
        XedocAccessToken::AgentIdentityJwt("header.payload.signature")
    ));
}
