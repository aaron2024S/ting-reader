use super::*;

fn signed_claims(exp: usize, secret: &str) -> String {
    encode(
        &Header::default(),
        &Claims {
            user_id: "user".into(),
            exp,
        },
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap()
}

#[test]
fn expired_and_malformed_tokens_have_distinct_errors() {
    let expired = signed_claims(1, "secret");
    assert!(
        validate_token(&expired, "secret")
            .unwrap_err()
            .to_string()
            .contains("令牌已过期")
    );
    assert!(
        validate_token_with_secrets(&expired, &["secret".into(), "rotated".into()])
            .unwrap_err()
            .to_string()
            .contains("令牌已过期")
    );
    assert!(
        validate_token("not.a.jwt", "secret")
            .unwrap_err()
            .to_string()
            .contains("令牌格式无效")
    );
}

#[test]
fn invalid_signatures_are_rejected_and_old_keys_still_work() {
    let token = generate_token("user", "old-secret").unwrap();
    assert!(
        validate_token(&token, "wrong")
            .unwrap_err()
            .to_string()
            .contains("令牌签名无效")
    );
    assert_eq!(
        validate_token_with_secrets(&token, &["new-secret".into(), "old-secret".into()])
            .unwrap()
            .user_id,
        "user"
    );
    assert!(validate_token_with_secrets(&token, &[]).is_err());
}
