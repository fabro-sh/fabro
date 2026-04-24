#![expect(
    clippy::disallowed_methods,
    reason = "integration tests that exercise sync dev-token file operations"
)]

use std::fs;

use fabro_util::dev_token::{
    DEV_TOKEN_PREFIX, generate_dev_token, read_dev_token_or_err,
    read_or_mint_dev_token_for_install, validate_dev_token_format,
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
fn read_or_mint_for_install_creates_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dev-token");

    let token = read_or_mint_dev_token_for_install(&path).unwrap();

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
fn read_or_mint_for_install_reads_existing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dev-token");
    let token = format!("{DEV_TOKEN_PREFIX}{}", "cd".repeat(32));
    fs::write(&path, &token).unwrap();

    let loaded = read_or_mint_dev_token_for_install(&path).unwrap();

    assert_eq!(loaded, token);
}

#[test]
fn read_dev_token_or_err_rejects_malformed_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dev-token");
    fs::write(&path, "not-a-token").unwrap();

    let error = read_dev_token_or_err(&path).unwrap_err();

    assert!(error.to_string().contains("invalid"));
}

#[test]
fn read_dev_token_or_err_reports_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing-dev-token");

    let error = read_dev_token_or_err(&path).unwrap_err();

    assert!(error.to_string().contains("read dev token"));
}
