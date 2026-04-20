#![expect(
    clippy::disallowed_methods,
    reason = "integration tests that exercise sync dev-token file operations"
)]

use std::fs;

use fabro_util::Home;
use fabro_util::dev_token::{
    DEV_TOKEN_PREFIX, generate_dev_token, load_or_create_dev_token, validate_dev_token_format,
};

#[test]
fn generate_has_correct_prefix_and_length() {
    let token = generate_dev_token();

    assert!(token.starts_with(DEV_TOKEN_PREFIX));
    assert_eq!(token.len(), 74);
    assert!(validate_dev_token_format(&token));
}

#[test]
fn generate_is_unique() {
    assert_ne!(generate_dev_token(), generate_dev_token());
}

#[test]
fn validate_format_accepts_valid() {
    let token = format!("{DEV_TOKEN_PREFIX}{}", "ab".repeat(32));

    assert!(validate_dev_token_format(&token));
}

#[test]
fn validate_format_rejects_short() {
    let token = format!("{DEV_TOKEN_PREFIX}{}", "ab".repeat(31));

    assert!(!validate_dev_token_format(&token));
}

#[test]
fn validate_format_rejects_non_hex() {
    let token = format!("{DEV_TOKEN_PREFIX}{}zz", "ab".repeat(31));

    assert!(!validate_dev_token_format(&token));
}

#[test]
fn validate_format_rejects_wrong_prefix() {
    let token = format!("fabro_nope_{}", "ab".repeat(32));

    assert!(!validate_dev_token_format(&token));
}

#[test]
fn load_or_create_creates_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dev-token");

    let token = load_or_create_dev_token(&path).unwrap();

    assert!(validate_dev_token_format(&token));
    assert_eq!(fs::read_to_string(&path).unwrap(), token);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn load_or_create_reads_existing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dev-token");
    let token = format!("{DEV_TOKEN_PREFIX}{}", "cd".repeat(32));
    fs::write(&path, &token).unwrap();

    let loaded = load_or_create_dev_token(&path).unwrap();

    assert_eq!(loaded, token);
}

#[test]
fn load_or_create_rejects_malformed_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dev-token");
    fs::write(&path, "not-a-token").unwrap();

    let error = load_or_create_dev_token(&path).unwrap_err();

    assert!(error.to_string().contains("invalid"));
}

#[test]
fn home_dev_token_path_is_relative_to_root() {
    let home = Home::new("/tmp/fabro-home");

    assert_eq!(
        home.dev_token_path(),
        std::path::Path::new("/tmp/fabro-home/dev-token")
    );
}
