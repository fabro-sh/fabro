use std::io;

use axum::http::HeaderValue;
use fabro_petri::artifacts::{ArtifactWriter, ClientArtifactWriter};
use fabro_types::ARTIFACT_MAX_FILE_BYTES;

use super::*;

fn upload(run: RunId, digest: &str, token: &str, body: Body) -> Request<Body> {
    let mut request = bearer_request(
        Method::PUT,
        &format!("/runs/{run}/artifacts/content/{digest}"),
        token,
        body,
    );
    request.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    request
}

#[tokio::test]
async fn artifact_upload_accepts_large_and_exact_limit_content_without_sqlite_or_listing_entries() {
    let (state, app) = jwt_auth_app();
    let user = issue_test_user_jwt();
    let run = create_run_with_bearer(&app, &user).await;
    let worker = issue_test_worker_token(&run);
    for size in [3 * 1024 * 1024, ARTIFACT_MAX_FILE_BYTES] {
        let bytes = vec![0xa3; size];
        let hash = BlobHash::new(&bytes);
        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(upload(
                    run,
                    &hash.to_string(),
                    &worker,
                    Body::from(bytes.clone()),
                ))
                .await
                .unwrap();
            assert_status!(response, StatusCode::NO_CONTENT).await;
        }
        assert_eq!(
            state
                .artifact_store
                .get_capture(&run, &hash)
                .await
                .unwrap()
                .unwrap(),
            bytes
        );
        assert!(
            state
                .store_ref()
                .blobs()
                .read(&hash)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(
        state
            .artifact_store
            .list_for_run(&run)
            .await
            .unwrap()
            .is_empty()
    );
    let response = app
        .oneshot(bearer_request(
            Method::GET,
            &format!("/runs/{run}/artifacts"),
            &user,
            Body::empty(),
        ))
        .await
        .unwrap();
    let body = response_json!(response, StatusCode::OK).await;
    assert_eq!(body["data"], json!([]));
}

#[tokio::test]
async fn artifact_upload_rejects_unauthorized_invalid_and_oversized_bodies_without_overwriting() {
    let (state, app) = jwt_auth_app();
    let user = issue_test_user_jwt();
    let run = create_run_with_bearer(&app, &user).await;
    let worker = issue_test_worker_token(&run);
    let other_worker = issue_test_worker_token(&RunId::new());
    let hash = BlobHash::new(b"valid content");
    state
        .artifact_store
        .put_capture(&run, &hash, b"valid content")
        .await
        .unwrap();
    let digest = hash.to_string();
    for (token, expected) in [
        (&user, StatusCode::FORBIDDEN),
        (&other_worker, StatusCode::FORBIDDEN),
    ] {
        let response = app
            .clone()
            .oneshot(upload(run, &digest, token, Body::from("invalid content")))
            .await
            .unwrap();
        assert_status!(response, expected).await;
    }
    let mut missing = upload(run, &digest, &worker, Body::empty());
    missing.headers_mut().remove(header::AUTHORIZATION);
    assert_status!(
        app.clone().oneshot(missing).await.unwrap(),
        StatusCode::UNAUTHORIZED
    )
    .await;
    for (digest, bytes) in [(&digest[..], "mismatch"), ("invalid", "valid content")] {
        let response = app
            .clone()
            .oneshot(upload(run, digest, &worker, Body::from(bytes)))
            .await
            .unwrap();
        assert_status!(response, StatusCode::BAD_REQUEST).await;
    }
    // Stream chunks without a content length: enforce bytes actually read.
    let chunks = futures_util::stream::iter([
        Ok::<_, io::Error>(Bytes::from(vec![0; ARTIFACT_MAX_FILE_BYTES])),
        Ok(Bytes::from_static(b"x")),
    ]);
    let response = app
        .clone()
        .oneshot(upload(run, &digest, &worker, Body::from_stream(chunks)))
        .await
        .unwrap();
    assert_status!(response, StatusCode::PAYLOAD_TOO_LARGE).await;
    let broken = futures_util::stream::iter([
        Ok(Bytes::from_static(b"valid")),
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated test body",
        )),
    ]);
    let response = app
        .clone()
        .oneshot(upload(run, &digest, &worker, Body::from_stream(broken)))
        .await
        .unwrap();
    assert_status!(response, StatusCode::BAD_REQUEST).await;
    let missing_run = RunId::new();
    let response = app
        .clone()
        .oneshot(upload(
            missing_run,
            &digest,
            &issue_test_worker_token(&missing_run),
            Body::from("valid content"),
        ))
        .await
        .unwrap();
    assert_status!(response, StatusCode::NOT_FOUND).await;
    run_records::append(&state, run, PlatformRecord::RunArchived)
        .await
        .unwrap();
    let response = app
        .oneshot(upload(run, &digest, &worker, Body::from("valid content")))
        .await
        .unwrap();
    assert_status!(response, StatusCode::CONFLICT).await;

    assert_eq!(
        state
            .artifact_store
            .get_capture(&run, &hash)
            .await
            .unwrap()
            .unwrap()
            .as_ref(),
        b"valid content"
    );
    assert!(
        state
            .store_ref()
            .blobs()
            .read(&hash)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn artifact_worker_client_uses_the_configured_s3_backend_and_prefix() {
    let s3 = MockServer::start_async().await;
    let settings = fabro_types::settings::server::ObjectStoreSettings::S3 {
        bucket:     "capture-bucket".to_string(),
        region:     "us-east-1".to_string(),
        endpoint:   Some(s3.base_url()),
        path_style: true,
    };
    let objects = crate::serve::build_object_store_from_settings_with_lookup(
        &settings,
        &|name| match name {
            "AWS_ACCESS_KEY_ID" => Some("fake-access-key".to_string()),
            "AWS_SECRET_ACCESS_KEY" => Some("fake-secret-key".to_string()),
            _ => None,
        },
        Some(&crate::serve::ObjectStoreBuildOptions {
            client_options: object_store::ClientOptions::new().with_allow_http(true),
            retry_config:   object_store::RetryConfig {
                max_retries: 0,
                ..Default::default()
            },
        }),
    )
    .unwrap();
    let (database, _) = test_store_bundle();
    let state = TestAppStateBuilder::new()
        .vault_entries([("OPENAI_API_KEY", "test-openai-api-key")])
        .server_secret_env(HashMap::from([(
            "SESSION_SECRET".to_string(),
            TEST_SESSION_SECRET.to_string(),
        )]))
        .store_bundle(database, ArtifactStore::new(objects, "selected-prefix"))
        .build();
    let app = build_router(state.clone(), jwt_auth_mode());
    let run = create_run_with_bearer(&app, &issue_test_user_jwt()).await;
    let bytes = vec![0x82; 3 * 1024 * 1024];
    let hash = BlobHash::new(&bytes);
    let path = format!("/capture-bucket/selected-prefix/{run}/captures/sha256/{hash}");
    let expected = bytes.clone();
    let put = s3
        .mock_async(|when, then| {
            when.method(httpmock::Method::PUT)
                .path(&path)
                .is_true(move |request| request.body_ref() == expected.as_slice());
            then.status(200).header("etag", "\"test-etag\"");
        })
        .await;
    let get = s3
        .mock_async(|when, then| {
            when.method(GET).path(&path);
            then.status(200)
                .header("etag", "\"test-etag\"")
                .header("last-modified", "Thu, 24 Sep 2026 12:00:00 GMT")
                .body(bytes.clone());
        })
        .await;
    let server = WorkerControlWsTestServer::spawn(app).await;
    let worker_token = issue_test_worker_token(&run);
    let mut headers = fabro_http::header::HeaderMap::new();
    headers.insert(
        fabro_http::header::AUTHORIZATION,
        format!("Bearer {worker_token}").parse().unwrap(),
    );
    let transport = fabro_http::HttpClientBuilder::new()
        .no_proxy()
        .default_headers(headers)
        .build()
        .unwrap();

    let client = fabro_client::Client::builder()
        .transport(server.base_url.replacen("ws://", "http://", 1), transport)
        .credential(fabro_client::Credential::Worker(worker_token))
        .connect()
        .await
        .unwrap();
    let writer = ClientArtifactWriter::new(client, run);
    writer.write(&hash, &bytes).await.unwrap();
    assert_eq!(
        state
            .artifact_store
            .get_capture(&run, &hash)
            .await
            .unwrap()
            .unwrap(),
        bytes
    );
    assert!(
        state
            .store_ref()
            .blobs()
            .read(&hash)
            .await
            .unwrap()
            .is_none()
    );
    put.assert_async().await;
    get.assert_async().await;
}
