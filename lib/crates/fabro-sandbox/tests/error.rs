#[test]
fn context_error_preserves_source_cause() {
    let source = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");

    let error = fabro_sandbox::Error::context("Failed to read file", source);

    assert_eq!(error.to_string(), "Failed to read file");
    assert_eq!(error.causes(), vec!["permission denied"]);
    assert_eq!(
        error.display_with_causes(),
        "Failed to read file\n  caused by: permission denied"
    );
}

#[cfg(feature = "docker")]
#[test]
fn docker_image_inspect_error_preserves_source_cause() {
    use bollard::errors::Error as BollardError;

    let source = BollardError::DockerResponseServerError {
        status_code: 500,
        message:     "daemon unavailable".to_string(),
    };

    let error = fabro_sandbox::Error::docker_image_inspect("buildpack-deps:noble", source);

    assert_eq!(
        error.to_string(),
        "Failed to inspect Docker image buildpack-deps:noble"
    );
    assert_eq!(error.causes(), vec![
        "Docker responded with status code 500: daemon unavailable"
    ]);
}
