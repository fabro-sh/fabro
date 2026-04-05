#[cfg(test)]
pub fn test_rsa_private_key() -> &'static str {
    include_str!("testdata/rsa_private.pem")
}

#[cfg(test)]
pub fn test_rsa_public_key() -> &'static str {
    include_str!("testdata/rsa_public.pem")
}

#[cfg(test)]
pub fn sign_test_jwt(app_id: &str, private_key_pem: &str) -> String {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde::Serialize;

    #[derive(Serialize)]
    struct Claims {
        iss: String,
        iat: i64,
        exp: i64,
    }

    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        iss: app_id.to_string(),
        iat: now - 60,
        exp: now + 600,
    };
    let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes()).unwrap();
    encode(&Header::new(Algorithm::RS256), &claims, &key).unwrap()
}
