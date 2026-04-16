use rand::Rng;

const MIN_SESSION_SECRET_LEN: usize = 64;

pub fn generate_session_secret() -> String {
    let mut rng = rand::rng();
    let bytes: [u8; 32] = rng.random();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;

        write!(&mut output, "{byte:02x}").expect("writing to string should not fail");
    }
    output
}

pub fn validate_session_secret(value: &str) -> Result<(), String> {
    if value.len() < MIN_SESSION_SECRET_LEN {
        return Err(format!(
            "too short ({} chars, need at least {} hex chars for 256-bit entropy)",
            value.len(),
            MIN_SESSION_SECRET_LEN,
        ));
    }
    if !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("contains non-hex characters".to_string());
    }
    Ok(())
}
