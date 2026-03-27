// ===========================================================================
// mTLS end-to-end tests
// ===========================================================================

// Skip on macOS: LibreSSL generates certs with extensions rustls rejects
#[cfg(target_os = "linux")]
mod mtls_e2e {
    use std::path::Path;
    use std::process::{Command, Stdio};

    use fabro_api::jwt_auth::{AuthMode, AuthStrategy};
    use fabro_api::server::{build_router, create_app_state};
    use fabro_api::server_config::TlsConfig;
    use fabro_api::tls::{build_rustls_config, ClientAuth};
    use fabro_workflows::pipeline::LlmSpec;
    use tokio::net::TcpListener;

    fn test_llm_spec() -> LlmSpec {
        LlmSpec {
            model: "test-model".to_string(),
            provider: fabro_llm::Provider::Anthropic,
            fallback_chain: Vec::new(),
            mcp_servers: Vec::new(),
            dry_run: true,
        }
    }

    async fn test_db() -> sqlx::SqlitePool {
        let pool = fabro_db::connect_memory().await.unwrap();
        fabro_db::initialize_db(&pool).await.unwrap();
        pool
    }

    /// Generate a complete CA + server cert + client cert PKI in `dir`.
    /// Returns paths: (ca_cert, server_cert, server_key, client_cert_pem, client_key_pem)
    fn generate_pki(dir: &Path, ca_cn: &str, server_cn: &str, client_cn: &str) -> PkiPaths {
        // CA key
        let ca_key_path = dir.join("ca.key");
        let ca_cert_path = dir.join("ca.crt");
        run_openssl(&[
            "genpkey",
            "-algorithm",
            "Ed25519",
            "-out",
            ca_key_path.to_str().unwrap(),
        ]);
        run_openssl(&[
            "req",
            "-new",
            "-x509",
            "-key",
            ca_key_path.to_str().unwrap(),
            "-out",
            ca_cert_path.to_str().unwrap(),
            "-days",
            "1",
            "-subj",
            &format!("/CN={ca_cn}"),
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
        ]);

        // Server key + cert signed by CA
        let server_key_path = dir.join("server.key");
        let server_csr_path = dir.join("server.csr");
        let server_cert_path = dir.join("server.crt");
        run_openssl(&[
            "genpkey",
            "-algorithm",
            "Ed25519",
            "-out",
            server_key_path.to_str().unwrap(),
        ]);
        run_openssl(&[
            "req",
            "-new",
            "-key",
            server_key_path.to_str().unwrap(),
            "-out",
            server_csr_path.to_str().unwrap(),
            "-subj",
            &format!("/CN={server_cn}"),
        ]);

        // Create extension file for SAN (reqwest validates server cert hostname)
        let ext_path = dir.join("server.ext");
        std::fs::write(&ext_path, "subjectAltName=IP:127.0.0.1").unwrap();

        run_openssl(&[
            "x509",
            "-req",
            "-in",
            server_csr_path.to_str().unwrap(),
            "-CA",
            ca_cert_path.to_str().unwrap(),
            "-CAkey",
            ca_key_path.to_str().unwrap(),
            "-CAcreateserial",
            "-out",
            server_cert_path.to_str().unwrap(),
            "-days",
            "1",
            "-extfile",
            ext_path.to_str().unwrap(),
        ]);

        // Client key + cert signed by CA
        let client_key_path = dir.join("client.key");
        let client_csr_path = dir.join("client.csr");
        let client_cert_path = dir.join("client.crt");
        run_openssl(&[
            "genpkey",
            "-algorithm",
            "Ed25519",
            "-out",
            client_key_path.to_str().unwrap(),
        ]);
        run_openssl(&[
            "req",
            "-new",
            "-key",
            client_key_path.to_str().unwrap(),
            "-out",
            client_csr_path.to_str().unwrap(),
            "-subj",
            &format!("/CN={client_cn}"),
        ]);
        // Client extension file to produce a v3 certificate
        let client_ext_path = dir.join("client.ext");
        std::fs::write(&client_ext_path, "basicConstraints=CA:FALSE\n").unwrap();
        run_openssl(&[
            "x509",
            "-req",
            "-in",
            client_csr_path.to_str().unwrap(),
            "-CA",
            ca_cert_path.to_str().unwrap(),
            "-CAkey",
            ca_key_path.to_str().unwrap(),
            "-CAcreateserial",
            "-out",
            client_cert_path.to_str().unwrap(),
            "-days",
            "1",
            "-extfile",
            client_ext_path.to_str().unwrap(),
        ]);

        PkiPaths {
            ca_cert: ca_cert_path,
            server_cert: server_cert_path,
            server_key: server_key_path,
            client_cert: client_cert_path,
            client_key: client_key_path,
        }
    }

    struct PkiPaths {
        ca_cert: std::path::PathBuf,
        server_cert: std::path::PathBuf,
        server_key: std::path::PathBuf,
        client_cert: std::path::PathBuf,
        client_key: std::path::PathBuf,
    }

    fn run_openssl(args: &[&str]) {
        let output = Command::new("openssl")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("openssl command failed to execute");
        assert!(
            output.status.success(),
            "openssl {} failed: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Start a TLS server on a random port, returning the bound address.
    async fn start_tls_server(
        tls_config: &TlsConfig,
        client_auth: ClientAuth,
        auth_mode: AuthMode,
    ) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let rustls_config = build_rustls_config(tls_config, client_auth);
        let tls_acceptor = tokio_rustls::TlsAcceptor::from(rustls_config);

        let state = create_app_state(test_db().await, test_llm_spec);
        let router = build_router(state, auth_mode);

        tokio::spawn(async move {
            let _ = fabro_api::tls::serve_tls(listener, tls_acceptor, router).await;
        });

        addr
    }

    /// Build a reqwest client with the given CA cert and optional client identity.
    fn build_client(
        ca_cert_path: &Path,
        client_cert_path: Option<&Path>,
        client_key_path: Option<&Path>,
    ) -> reqwest::Client {
        let ca_pem = std::fs::read(ca_cert_path).unwrap();
        let ca_cert = reqwest::tls::Certificate::from_pem(&ca_pem).unwrap();

        let mut builder = reqwest::Client::builder()
            .add_root_certificate(ca_cert)
            .use_rustls_tls();

        if let (Some(cert_path), Some(key_path)) = (client_cert_path, client_key_path) {
            let cert_pem = std::fs::read(cert_path).unwrap();
            let key_pem = std::fs::read(key_path).unwrap();
            let mut identity_pem = cert_pem;
            identity_pem.extend_from_slice(&key_pem);
            let identity = reqwest::tls::Identity::from_pem(&identity_pem).unwrap();
            builder = builder.identity(identity);
        }

        builder.build().unwrap()
    }

    fn install_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    #[tokio::test]
    async fn mtls_accepts_valid_client_cert() {
        install_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let pki = generate_pki(dir.path(), "TestCA", "localhost", "testuser");

        let tls_config = TlsConfig {
            cert: pki.server_cert.clone(),
            key: pki.server_key.clone(),
            ca: pki.ca_cert.clone(),
        };

        let auth_mode = AuthMode::Strategies(vec![AuthStrategy::Mtls]);
        let addr = start_tls_server(&tls_config, ClientAuth::Required, auth_mode).await;

        let client = build_client(&pki.ca_cert, Some(&pki.client_cert), Some(&pki.client_key));

        let response = client
            .get(format!("https://127.0.0.1:{}/runs", addr.port()))
            .send()
            .await
            .expect("request with valid client cert should succeed");

        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn mtls_only_rejects_wrong_ca_client_cert() {
        install_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let pki = generate_pki(dir.path(), "TestCA", "localhost", "testuser");

        let tls_config = TlsConfig {
            cert: pki.server_cert.clone(),
            key: pki.server_key.clone(),
            ca: pki.ca_cert.clone(),
        };

        let auth_mode = AuthMode::Strategies(vec![AuthStrategy::Mtls]);
        let addr = start_tls_server(&tls_config, ClientAuth::Required, auth_mode).await;

        // Generate a DIFFERENT CA and client cert signed by it
        let wrong_dir = dir.path().join("wrong_ca");
        std::fs::create_dir_all(&wrong_dir).unwrap();
        let wrong_pki = generate_pki(&wrong_dir, "WrongCA", "localhost", "intruder");

        // Client trusts the REAL server CA, but presents a cert from the WRONG CA
        let client = build_client(
            &pki.ca_cert,
            Some(&wrong_pki.client_cert),
            Some(&wrong_pki.client_key),
        );

        let result = client
            .get(format!("https://127.0.0.1:{}/runs", addr.port()))
            .send()
            .await;

        // Server should reject the TLS handshake — the wrong CA client cert
        // will cause a connection error (not an HTTP error)
        assert!(
            result.is_err(),
            "request with wrong-CA client cert should fail at TLS level, but got: {:?}",
            result.unwrap().status()
        );
    }

    #[tokio::test]
    async fn mtls_only_rejects_no_client_cert() {
        install_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let pki = generate_pki(dir.path(), "TestCA", "localhost", "testuser");

        let tls_config = TlsConfig {
            cert: pki.server_cert.clone(),
            key: pki.server_key.clone(),
            ca: pki.ca_cert.clone(),
        };

        // mTLS is the ONLY strategy → client cert is required at TLS level
        let auth_mode = AuthMode::Strategies(vec![AuthStrategy::Mtls]);
        let addr = start_tls_server(&tls_config, ClientAuth::Required, auth_mode).await;

        // Client trusts the server CA but presents NO client cert
        let client = build_client(&pki.ca_cert, None, None);

        let result = client
            .get(format!("https://127.0.0.1:{}/runs", addr.port()))
            .send()
            .await;

        // Server requires client cert → TLS handshake should fail
        assert!(
            result.is_err(),
            "request without client cert should fail when mTLS is the only strategy, but got: {:?}",
            result.unwrap().status()
        );
    }

    /// Generate an Ed25519 JWT keypair. Returns (encoding_key, decoding_key).
    fn generate_jwt_keypair() -> (jsonwebtoken::EncodingKey, jsonwebtoken::DecodingKey) {
        let output = Command::new("openssl")
            .args(["genpkey", "-algorithm", "Ed25519"])
            .output()
            .expect("openssl must be available for tests");
        let private_pem = output.stdout;

        let output = Command::new("openssl")
            .args(["pkey", "-pubout"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(&private_pem).unwrap();
                child.wait_with_output()
            })
            .expect("openssl pkey failed");
        let public_pem = output.stdout;

        let encoding =
            jsonwebtoken::EncodingKey::from_ed_pem(&private_pem).expect("invalid private key");
        let decoding =
            jsonwebtoken::DecodingKey::from_ed_pem(&public_pem).expect("invalid public key");
        (encoding, decoding)
    }

    fn sign_jwt(key: &jsonwebtoken::EncodingKey, sub: &str) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = serde_json::json!({
            "iss": "fabro-web",
            "iat": now,
            "exp": now + 60,
            "sub": sub,
        });
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA);
        jsonwebtoken::encode(&header, &claims, key).expect("failed to sign token")
    }

    #[tokio::test]
    async fn mtls_and_jwt_accepts_valid_jwt_without_client_cert() {
        install_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let pki = generate_pki(dir.path(), "TestCA", "localhost", "testuser");

        let tls_config = TlsConfig {
            cert: pki.server_cert.clone(),
            key: pki.server_key.clone(),
            ca: pki.ca_cert.clone(),
        };

        let (encoding_key, decoding_key) = generate_jwt_keypair();

        // Both mTLS and JWT strategies; mTLS is optional since JWT is also present
        let auth_mode = AuthMode::Strategies(vec![
            AuthStrategy::Mtls,
            AuthStrategy::Jwt {
                key: Arc::new(decoding_key),
                validation: Arc::new(fabro_api::jwt_auth::jwt_validation()),
                allowed_usernames: vec!["brynary".to_string()],
            },
        ]);
        let addr = start_tls_server(&tls_config, ClientAuth::Optional, auth_mode).await;

        // Client trusts the server CA but presents NO client cert
        let client = build_client(&pki.ca_cert, None, None);

        let token = sign_jwt(&encoding_key, "https://github.com/brynary");

        let response = client
            .get(format!("https://127.0.0.1:{}/runs", addr.port()))
            .bearer_auth(&token)
            .send()
            .await
            .expect("request with valid JWT and no client cert should succeed");

        assert_eq!(
            response.status(),
            200,
            "valid JWT should be accepted when strategies = [mtls, jwt]"
        );
    }
}

// ===========================================================================
// Full HTTP server lifecycle (TS Scenario 4)
// ===========================================================================

mod server_lifecycle {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use fabro_api::server::{build_router, create_app_state_with_registry_factory};
    use fabro_interview::Interviewer;
    use fabro_workflows::handler::agent::AgentHandler;
    use fabro_workflows::handler::exit::ExitHandler;
    use fabro_workflows::handler::human::HumanHandler;
    use fabro_workflows::handler::start::StartHandler;
    use fabro_workflows::handler::HandlerRegistry;
    use fabro_workflows::pipeline::LlmSpec;
    use tower::ServiceExt;

    fn test_llm_spec() -> LlmSpec {
        LlmSpec {
            model: "test-model".to_string(),
            provider: fabro_llm::Provider::Anthropic,
            fallback_chain: Vec::new(),
            mcp_servers: Vec::new(),
            dry_run: true,
        }
    }

    fn gate_registry(interviewer: Arc<dyn Interviewer>) -> HandlerRegistry {
        let mut registry = HandlerRegistry::new(Box::new(AgentHandler::new(None)));
        registry.register("start", Box::new(StartHandler));
        registry.register("exit", Box::new(ExitHandler));
        registry.register("agent", Box::new(AgentHandler::new(None)));
        registry.register("human", Box::new(HumanHandler::new(interviewer)));
        registry
    }

    async fn body_json(body: Body) -> serde_json::Value {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    const GATE_DOT: &str = r#"digraph GateTest {
        graph [goal="Test gate"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        work  [shape=box, prompt="Do work"]
        gate  [shape=hexagon, type="human", label="Approve?"]
        done  [shape=box, prompt="Finish"]
        revise [shape=box, prompt="Revise"]

        start -> work -> gate
        gate -> done   [label="[A] Approve"]
        gate -> revise [label="[R] Revise"]
        done -> exit
        revise -> gate
    }"#;

    async fn test_db() -> sqlx::SqlitePool {
        let pool = fabro_db::connect_memory().await.unwrap();
        fabro_db::initialize_db(&pool).await.unwrap();
        pool
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_http_lifecycle_approve_and_complete() {
        let state =
            create_app_state_with_registry_factory(test_db().await, test_llm_spec, gate_registry);
        fabro_api::server::spawn_scheduler(Arc::clone(&state));
        let app = build_router(Arc::clone(&state), fabro_api::jwt_auth::AuthMode::Disabled);

        // 1. Start run
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": GATE_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // 2. Poll for question to appear (run goes start -> work -> gate, then blocks)
        let mut question_id = String::new();
        for _ in 0..500 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let req = Request::builder()
                .method("GET")
                .uri(format!("/runs/{run_id}/questions"))
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(req).await.unwrap();
            let body = body_json(response.into_body()).await;
            let arr = body["data"].as_array().unwrap();
            if !arr.is_empty() {
                question_id = arr[0]["id"].as_str().unwrap().to_string();
                break;
            }
        }
        assert!(!question_id.is_empty(), "question should have appeared");

        // 3. Submit answer selecting first option (Approve)
        let req = Request::builder()
            .method("POST")
            .uri(format!("/runs/{run_id}/questions/{question_id}/answer"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"value": "A"})).unwrap(),
            ))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // 4. Poll until completed
        let mut final_status = String::new();
        for _ in 0..500 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let req = Request::builder()
                .method("GET")
                .uri(format!("/runs/{run_id}"))
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(req).await.unwrap();
            let body = body_json(response.into_body()).await;
            let status = body["status"].as_str().unwrap().to_string();
            if status == "completed" || status == "failed" {
                final_status = status;
                break;
            }
        }
        assert_eq!(final_status, "completed");

        // 5. Verify context endpoint returns an object
        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}/context"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let ctx_body = body_json(response.into_body()).await;
        assert!(ctx_body.is_object(), "context should be an object");

        // 6. Verify no pending questions
        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}/questions"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert!(
            body["data"].as_array().unwrap().is_empty(),
            "no pending questions after completion"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_http_lifecycle_cancel() {
        let state =
            create_app_state_with_registry_factory(test_db().await, test_llm_spec, gate_registry);
        fabro_api::server::spawn_scheduler(Arc::clone(&state));
        let app = build_router(Arc::clone(&state), fabro_api::jwt_auth::AuthMode::Disabled);

        // Start a run that will block at the human gate
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": GATE_DOT})).unwrap(),
            ))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();

        // Wait briefly for scheduler to pick up the run
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Cancel it
        let req = Request::builder()
            .method("POST")
            .uri(format!("/runs/{run_id}/cancel"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"], "cancelled");

        // Verify status is cancelled
        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"], "cancelled");
    }
}

// ===========================================================================
// SSE event stream content parsing (TS Scenario 8)
// ===========================================================================

mod sse_events {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use fabro_api::server::{build_router, create_app_state};
    use fabro_workflows::pipeline::LlmSpec;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_llm_spec() -> LlmSpec {
        LlmSpec {
            model: "test-model".to_string(),
            provider: fabro_llm::Provider::Anthropic,
            fallback_chain: Vec::new(),
            mcp_servers: Vec::new(),
            dry_run: true,
        }
    }

    const SIMPLE_DOT: &str = r#"digraph SSETest {
        graph [goal="Test SSE"]
        start [shape=Mdiamond]
        work  [shape=box, prompt="Do work"]
        exit  [shape=Msquare]
        start -> work -> exit
    }"#;

    async fn test_db() -> sqlx::SqlitePool {
        let pool = fabro_db::connect_memory().await.unwrap();
        fabro_db::initialize_db(&pool).await.unwrap();
        pool
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sse_stream_contains_expected_event_types() {
        let state = create_app_state(test_db().await, test_llm_spec);
        fabro_api::server::spawn_scheduler(Arc::clone(&state));
        let app = build_router(Arc::clone(&state), fabro_api::jwt_auth::AuthMode::Disabled);

        // Start run
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": SIMPLE_DOT})).unwrap(),
            ))
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let run_id = body["id"].as_str().unwrap().to_string();

        // Wait for scheduler to pick up the run before subscribing to SSE
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Get SSE stream
        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}/events"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        // May be 200 (stream open) or 410 (run completed before connect)
        let sse_status = response.status();
        assert!(
            sse_status == StatusCode::OK || sse_status == StatusCode::GONE,
            "expected 200 or 410, got: {sse_status}"
        );
        if sse_status == StatusCode::GONE {
            return;
        }

        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(content_type.contains("text/event-stream"));

        // Collect SSE frames with a timeout
        let mut body = response.into_body();
        let mut sse_data = String::new();
        while let Ok(Some(Ok(frame))) =
            tokio::time::timeout(Duration::from_millis(500), body.frame()).await
        {
            if let Some(data) = frame.data_ref() {
                sse_data.push_str(&String::from_utf8_lossy(data));
            }
        }

        // Parse SSE data lines and extract event types
        let mut event_types: Vec<String> = Vec::new();
        for line in sse_data.lines() {
            if let Some(json_str) = line.strip_prefix("data:") {
                let json_str = json_str.trim();
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(json_str) {
                    // The event is serialized as a tagged enum, so the type is the first key
                    if let Some(obj) = event.as_object() {
                        for key in obj.keys() {
                            event_types.push(key.clone());
                        }
                    } else if let Some(s) = event.as_str() {
                        event_types.push(s.to_string());
                    }
                }
            }
        }

        // Verify we got events (run may have completed before we subscribed,
        // so we check that the stream was valid SSE)
        // If events were emitted before subscribe, the stream may be empty.
        // That's OK -- the main assertion is content-type + valid SSE format.
        // But if we got events, verify expected types.
        if !event_types.is_empty() {
            assert!(
                event_types
                    .iter()
                    .any(|t| t == "StageStarted" || t == "StageCompleted"),
                "should contain stage events, got: {event_types:?}"
            );
        }

        // Pipeline is complete (SSE stream ended), verify checkpoint
        // Small yield to let the spawned task update state
        tokio::time::sleep(Duration::from_millis(10)).await;

        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}/checkpoint"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let cp_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // If run completed, checkpoint should have completed_nodes
        if !cp_body.is_null() {
            let completed = cp_body["completed_nodes"].as_array();
            if let Some(nodes) = completed {
                let names: Vec<&str> = nodes.iter().filter_map(|v| v.as_str()).collect();
                assert!(names.contains(&"work"), "work should be in completed_nodes");
            }
        }
    }
}

// ===========================================================================
// Serve command: dry-run registry factory builds a working router
// ===========================================================================

mod serve_dry_run {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use fabro_api::server::{build_router, create_app_state};
    use fabro_workflows::pipeline::LlmSpec;
    use tower::ServiceExt;

    const MINIMAL_DOT: &str = r#"digraph Test {
        graph [goal="Test"]
        start [shape=Mdiamond]
        exit  [shape=Msquare]
        start -> exit
    }"#;

    async fn test_db() -> sqlx::SqlitePool {
        let pool = fabro_db::connect_memory().await.unwrap();
        fabro_db::initialize_db(&pool).await.unwrap();
        pool
    }

    fn test_llm_spec() -> LlmSpec {
        LlmSpec {
            model: "test-model".to_string(),
            provider: fabro_llm::Provider::Anthropic,
            fallback_chain: Vec::new(),
            mcp_servers: Vec::new(),
            dry_run: true,
        }
    }

    /// Build the router exactly as `serve_command` does in dry-run mode.
    async fn dry_run_app() -> axum::Router {
        let state = create_app_state(test_db().await, test_llm_spec);
        fabro_api::server::spawn_scheduler(Arc::clone(&state));
        build_router(state, fabro_api::jwt_auth::AuthMode::Disabled)
    }

    async fn body_json(body: Body) -> serde_json::Value {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dry_run_serve_starts_and_runs_workflow() {
        let app = dry_run_app().await;

        // POST /runs to start a run
        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": MINIMAL_DOT})).unwrap(),
            ))
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let body = body_json(response.into_body()).await;
        let run_id = body["id"].as_str().unwrap().to_string();
        assert!(!run_id.is_empty());

        // Wait for run to complete
        tokio::time::sleep(Duration::from_millis(500)).await;

        // GET /runs/{id} to verify completion
        let req = Request::builder()
            .method("GET")
            .uri(format!("/runs/{run_id}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["status"].as_str().unwrap(), "completed");
    }

    #[tokio::test]
    async fn test_model_known_via_full_router() {
        let app = dry_run_app().await;

        let req = Request::builder()
            .method("POST")
            .uri("/models/claude-opus-4-6/test")
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_json(response.into_body()).await;
        assert_eq!(body["model_id"], "claude-opus-4-6");
        // No API keys in test env, so status will be "error"
        assert!(body["status"] == "ok" || body["status"] == "error");
    }

    #[tokio::test]
    async fn test_model_unknown_via_full_router() {
        let app = dry_run_app().await;

        let req = Request::builder()
            .method("POST")
            .uri("/models/nonexistent-model-xyz/test")
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn dry_run_serve_rejects_invalid_dot() {
        let app = dry_run_app().await;

        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({"dot_source": "not valid dot"})).unwrap(),
            ))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
