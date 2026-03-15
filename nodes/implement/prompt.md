Goal: # Add `goal` to WorkflowRunStarted and render in `fabro logs --pretty`

## Context

`fabro logs --pretty` shows `▶ WorkflowName  run_id` at the top but doesn't show what the workflow is trying to do. The goal is available via `graph.goal()` at the emit site but isn't included in the event. Adding it gives users immediate context when reading logs.

Goals can be short one-liners or full markdown documents.

## Changes

### 1. Add `goal` field to `WorkflowRunEvent::WorkflowRunStarted` (`event.rs:~11`)

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
goal: Option<String>,
```

`Option` + `serde(default)` for backward compat with old JSONL (same pattern as `base_sha`, `run_branch`).

### 2. Populate at emit site (`engine.rs:~1209`)

Add `goal:` field using `graph.goal()`. Emit `None` when empty, `Some(...)` otherwise:

```rust
goal: {
    let g = graph.goal();
    if g.is_empty() { None } else { Some(g.to_string()) }
},
```

### 3. Update all other construction sites

Grep for `WorkflowRunStarted {` — tests in `event.rs` construct this variant. Add `goal: None` to each.

### 4. Render in `logs.rs` `format_event_pretty` (`logs.rs:~203`)

After the existing header line, if `goal` is present, render it below indented. Use `styles.render_markdown_width()` for markdown rendering (same approach as `Agent.AssistantMessage` at line 355).

- Short goal (single line, no markdown): render inline on same line or as a single indented line
- Multi-line / markdown goal: render with `render_markdown_width` + indent, same as assistant messages

```rust
"WorkflowRunStarted" => {
    let name = str_field(&envelope, "workflow_name").unwrap_or("?");
    let run_id = str_field(&envelope, "run_id").unwrap_or("?");
    let header = format!(
        "{} {} {}  {}",
        styles.dim.apply_to(&ts),
        styles.bold_cyan.apply_to("\u{25b6}"),
        styles.bold.apply_to(name),
        styles.dim.apply_to(run_id),
    );
    match str_field(&envelope, "goal") {
        Some(goal) if !goal.is_empty() => {
            let indent = "            ";
            let term_width = fabro_util::terminal::Styles::terminal_width();
            let wrap_width = term_width.saturating_sub(indent.len());
            let rendered = styles.render_markdown_width(goal, wrap_width);
            let body: String = rendered
                .lines()
                .map(|l| format!("{indent}{l}"))
                .collect::<Vec<_>>()
                .join("\n");
            Some(format!("{header}\n{body}\n"))
        }
        _ => Some(header),
    }
}
```

### 5. Tests (`event.rs`)

- Update existing `workflow_run_started` serialization tests to include `goal`
- Add test: round-trip with `goal: Some("Fix the bug")`
- Existing backward-compat test (`workflow_run_completed_backward_compat_without_new_fields` pattern) — add one for `WorkflowRunStarted` without `goal` field deserializing to `goal: None`

## Files to modify

| File | What |
|---|---|
| `lib/crates/fabro-workflows/src/event.rs` | Add `goal` field, update trace, update tests |
| `lib/crates/fabro-workflows/src/engine.rs` | Populate `goal` at emit site |
| `lib/crates/fabro-workflows/src/cli/logs.rs` | Render goal in pretty logs |

## Verification

1. `cargo test -p fabro-workflows`
2. `cargo build --workspace`
3. `cargo test --workspace`
4. `cargo clippy --workspace -- -D warnings`
5. `cargo fmt --check --all`


## Completed stages
- **toolchain**: success
  - Script: `command -v cargo >/dev/null || { curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && sudo ln -sf $HOME/.cargo/bin/* /usr/local/bin/; }; cargo --version 2>&1`
  - Stdout:
    ```
    cargo 1.94.0 (85eff7c80 2026-01-15)
    ```
  - Stderr: (empty)
- **preflight_compile**: success
  - Script: `cargo check 2>&1`
  - Stdout:
    ```
    Updating crates.io index
    Updating git repository `https://github.com/brynary/daytona-sdk-rust`
 Downloading crates ...
  Downloaded anstyle v1.0.13
  Downloaded heck v0.5.0
  Downloaded anyhow v1.0.102
  Downloaded anstyle-query v1.1.5
  Downloaded getrandom v0.2.17
  Downloaded form_urlencoded v1.2.2
  Downloaded jsonwebtoken v10.3.0
  Downloaded litrs v1.0.0
  Downloaded parking_lot_core v0.9.12
  Downloaded pastey v0.2.1
  Downloaded rusticata-macros v4.1.0
  Downloaded strict v0.2.0
  Downloaded toml_datetime v0.6.11
  Downloaded toml_write v0.1.2
  Downloaded vsimd v0.8.0
  Downloaded webpki-roots v0.26.11
  Downloaded web_atoms v0.1.3
  Downloaded writeable v0.6.2
  Downloaded untrusted v0.9.0
  Downloaded unicode-general-category v1.1.0
  Downloaded zmij v1.0.21
  Downloaded zerofrom-derive v0.1.6
  Downloaded zerovec-derive v0.11.2
  Downloaded nix v0.31.2
  Downloaded time v0.3.47
  Downloaded syn v2.0.117
  Downloaded rustls v0.23.37
  Downloaded regex-syntax v0.8.10
  Downloaded unicode-segmentation v1.12.0
  Downloaded rustix v1.1.4
  Downloaded x509-parser v0.16.0
  Downloaded unicode-width v0.1.14
  Downloaded winnow v0.7.14
  Downloaded aws-lc-sys v0.38.0
  Downloaded vcpkg v0.2.15
  Downloaded webpki-roots v1.0.6
  Downloaded tower v0.5.3
  Downloaded tower-http v0.6.8
  Downloaded tracing-subscriber v0.3.22
  Downloaded libssh2-sys v0.3.1
  Downloaded zerotrie v0.2.3
  Downloaded zerovec v0.11.5
  Downloaded regex-automata v0.4.14
  Downloaded typenum v1.19.0
  Downloaded tracing v0.1.44
  Downloaded libc v0.2.182
  Downloaded url v2.5.8
  Downloaded unicode-width v0.2.2
  Downloaded libz-sys v1.1.24
  Downloaded htmd v0.5.0
  Downloaded yoke v0.8.1
  Downloaded xml5ever v0.35.0
  Downloaded zerocopy v0.8.40
  Downloaded tokio v1.49.0
  Downloaded termimad v0.34.1
  Downloaded openssl v0.10.75
  Downloaded yoke-derive v0.8.1
  Downloaded xattr v1.6.1
  Downloaded tower-service v0.3.3
  Downloaded nix v0.29.0
  Downloaded zeroize v1.8.2
  Downloaded zerofrom v0.1.6
  Downloaded rmcp v0.15.0
  Downloaded quinn-proto v0.11.13
  Downloaded markup5ever_rcdom v0.35.0+unofficial
  Downloaded ulid v1.2.1
  Downloaded unicode-ident v1.0.24
  Downloaded unicase v2.9.0
  Downloaded try-lock v0.2.5
  Downloaded tracing-appender v0.2.4
  Downloaded tokio-util v0.7.18
  Downloaded tempfile v3.26.0
  Downloaded process-wrap v9.0.3
  Downloaded uuid v1.21.0
  Downloaded untrusted v0.7.1
  Downloaded unit-prefix v0.5.2
  Downloaded tracing-core v0.1.36
  Downloaded tracing-attributes v0.1.31
  Downloaded reqwest v0.13.2
  Downloaded walkdir v2.5.0
  Downloaded unsafe-libyaml v0.2.11
  Downloaded tungstenite v0.26.2
  Downloaded thiserror-impl v1.0.69
  Downloaded thiserror v2.0.18
  Downloaded thiserror v1.0.69
  Downloaded subtle v2.6.1
  Downloaded strsim v0.11.1
  Downloaded ring v0.17.14
  Downloaded string_cache_codegen v0.5.4
  Downloaded serde_with v3.17.0
  Downloaded serde_json v1.0.149
  Downloaded serde v1.0.228
  Downloaded schemars v1.2.1
  Downloaded rustls-webpki v0.103.9
  Downloaded reqwest v0.12.28
  Downloaded regex v1.12.3
  Downloaded hyper v1.8.1
  Downloaded want v0.3.1
  Downloaded libgit2-sys v0.18.3+1.9.2
  Downloaded version_check v0.9.5
  Downloaded uuid-simd v0.8.0
  Downloaded utf8_iter v1.0.4
  Downloaded tracing-log v0.2.0
  Downloaded tower-layer v0.3.3
  Downloaded toml_edit v0.22.27
  Downloaded time-core v0.1.8
  Downloaded thread_local v1.1.9
  Downloaded thiserror-impl v2.0.18
  Downloaded tendril v0.4.3
  Downloaded tar v0.4.44
  Downloaded synstructure v0.13.2
  Downloaded schemars v0.9.0
  Downloaded jsonschema v0.42.2
  Downloaded utf8parse v0.2.2
  Downloaded utf-8 v0.7.6
  Downloaded tokio-rustls v0.26.4
  Downloaded tinyvec v1.10.0
  Downloaded sync_wrapper v1.0.2
  Downloaded sha2 v0.10.9
  Downloaded serde_with_macros v3.17.0
  Downloaded serde_urlencoded v0.7.1
  Downloaded serde_path_to_error v0.1.20
  Downloaded serde_derive_internals v0.29.1
  Downloaded serde_derive v1.0.228
  Downloaded serde_core v1.0.228
  Downloaded semver v1.0.27
  Downloaded schemars_derive v1.2.1
  Downloaded ryu v1.0.23
  Downloaded rand v0.9.2
  Downloaded rand v0.8.5
  Downloaded num-bigint v0.4.6
  Downloaded nom v7.1.3
  Downloaded hyper-util v0.1.20
  Downloaded toml v0.8.23
  Downloaded tokio-tungstenite v0.26.2
  Downloaded tokio-stream v0.1.18
  Downloaded tokio-native-tls v0.3.1
  Downloaded tokio-macros v2.6.0
  Downloaded tinyvec_macros v0.1.1
  Downloaded tinystr v0.8.2
  Downloaded time-macros v0.2.27
  Downloaded socket2 v0.6.2
  Downloaded smallvec v1.15.1
  Downloaded signal-hook v0.3.18
  Downloaded sharded-slab v0.1.7
  Downloaded serde_yaml v0.9.34+deprecated
  Downloaded serde_spanned v0.6.9
  Downloaded scopeguard v1.2.0
  Downloaded same-file v1.0.6
  Downloaded rustls-platform-verifier v0.6.2
  Downloaded rustls-pki-types v1.14.0
  Downloaded rustls-pemfile v2.2.0
  Downloaded rustls-native-certs v0.8.3
  Downloaded quinn v0.11.9
  Downloaded portable-atomic v1.13.1
  Downloaded openssl-sys v0.9.111
  Downloaded mio v1.1.1
  Downloaded memchr v2.8.0
  Downloaded iri-string v0.7.10
  Downloaded idna v1.1.0
  Downloaded futures-util v0.3.32
  Downloaded fancy-regex v0.17.0
  Downloaded derive_more v2.1.1
  Downloaded darling_core v0.23.0
  Downloaded crossbeam-channel v0.5.15
  Downloaded string_cache v0.8.9
  Downloaded sse-stream v0.2.1
  Downloaded slab v0.4.12
  Downloaded simple_asn1 v0.6.4
  Downloaded linux-raw-sys v0.12.1
  Downloaded signature v2.2.0
  Downloaded signal-hook-registry v1.4.8
  Downloaded signal-hook-mio v0.2.5
  Downloaded shlex v1.3.0
  Downloaded sha1 v0.10.6
  Downloaded serde_repr v0.1.20
  Downloaded reqwest-middleware v0.4.2
  Downloaded referencing v0.42.2
  Downloaded ref-cast-impl v1.0.25
  Downloaded ref-cast v1.0.25
  Downloaded rand_core v0.9.5
  Downloaded rand_core v0.6.4
  Downloaded rand_chacha v0.9.0
  Downloaded rand_chacha v0.3.1
  Downloaded parking_lot v0.12.5
  Downloaded openssh v0.11.6
  Downloaded num-traits v0.2.19
  Downloaded minimal-lexical v0.2.1
  Downloaded log v0.4.29
  Downloaded fraction v0.15.3
  Downloaded stable_deref_trait v1.2.1
  Downloaded siphasher v1.0.2
  Downloaded shell-words v1.1.1
  Downloaded shell-escape v0.1.5
  Downloaded rmcp-macros v0.15.0
  Downloaded quote v1.0.44
  Downloaded quinn-udp v0.5.14
  Downloaded proc-macro2 v1.0.106
  Downloaded powerfmt v0.2.0
  Downloaded potential_utf v0.1.4
  Downloaded pkg-config v0.3.32
  Downloaded pin-utils v0.1.0
  Downloaded pin-project-lite v0.2.17
  Downloaded phf_shared v0.13.1
  Downloaded phf_shared v0.11.3
  Downloaded phf_macros v0.13.1
  Downloaded phf_generator v0.13.1
  Downloaded phf_generator v0.11.3
  Downloaded phf v0.13.1
  Downloaded phf v0.11.3
  Downloaded pathdiff v0.2.3
  Downloaded open v5.3.3
  Downloaded num-rational v0.4.2
  Downloaded mime_guess v2.0.5
  Downloaded md5 v0.7.0
  Downloaded matchit v0.8.4
  Downloaded indexmap v2.13.0
  Downloaded icu_properties_data v2.1.2
  Downloaded httpdate v1.0.3
  Downloaded httparse v1.10.1
  Downloaded http-body-util v0.1.3
  Downloaded encoding_rs v0.8.35
  Downloaded bollard v0.18.1
  Downloaded rustc_version v0.4.1
  Downloaded rustc-hash v2.1.1
  Downloaded precomputed-hash v0.1.1
  Downloaded ppv-lite86 v0.2.21
  Downloaded phf_codegen v0.11.3
  Downloaded percent-encoding v2.3.2
  Downloaded pem v3.0.6
  Downloaded once_cell v1.21.3
  Downloaded num-complex v0.4.6
  Downloaded nu-ansi-term v0.50.3
  Downloaded mime v0.3.17
  Downloaded memoffset v0.9.1
  Downloaded matchers v0.2.0
  Downloaded match_token v0.35.0
  Downloaded mac_address v1.1.8
  Downloaded mac v0.1.1
  Downloaded lru-slab v0.1.2
  Downloaded lock_api v0.4.14
  Downloaded indicatif v0.18.4
  Downloaded http v1.4.0
  Downloaded futures-executor v0.3.32
  Downloaded fnv v1.0.7
  Downloaded fluent-uri v0.4.1
  Downloaded find-msvc-tools v0.1.9
  Downloaded fastrand v2.3.0
  Downloaded errno v0.3.14
  Downloaded email_address v0.2.9
  Downloaded dyn-clone v1.0.20
  Downloaded document-features v0.2.12
  Downloaded digest v0.10.7
  Downloaded deranged v0.5.8
  Downloaded darling_macro v0.21.3
  Downloaded darling_core v0.21.3
  Downloaded crossterm v0.29.0
  Downloaded console v0.15.11
  Downloaded clap_lex v1.0.0
  Downloaded clap_builder v4.5.60
  Downloaded chrono v0.4.44
  Downloaded cc v1.2.56
  Downloaded bitflags v2.11.0
  Downloaded outref v0.5.2
  Downloaded option-ext v0.2.0
  Downloaded openssl-probe v0.2.1
  Downloaded openssl-probe v0.1.6
  Downloaded openssl-macros v0.1.1
  Downloaded oid-registry v0.7.1
  Downloaded num-iter v0.1.45
  Downloaded num-conv v0.2.0
  Downloaded num-cmp v0.1.0
  Downloaded num v0.4.3
  Downloaded markup5ever v0.35.0
  Downloaded icu_normalizer_data v2.1.1
  Downloaded icu_locale_core v2.1.1
  Downloaded icu_collections v2.1.1
  Downloaded generic-array v0.14.7
  Downloaded futures-task v0.3.32
  Downloaded futures-sink v0.3.32
  Downloaded futures-macro v0.3.32
  Downloaded futures-channel v0.3.32
  Downloaded futures v0.3.32
  Downloaded fs_extra v1.3.0
  Downloaded foreign-types-shared v0.1.1
  Downloaded dirs v6.0.0
  Downloaded derive_more-impl v2.1.1
  Downloaded cpufeatures v0.2.17
  Downloaded convert_case v0.10.0
  Downloaded cfg-if v1.0.4
  Downloaded bollard-stubs v1.47.1-rc.27.3.1
  Downloaded bit-set v0.8.0
  Downloaded base64 v0.22.1
  Downloaded axum-core v0.5.6
  Downloaded num-integer v0.1.46
  Downloaded new_debug_unreachable v1.0.6
  Downloaded native-tls v0.2.18
  Downloaded minimad v0.14.0
  Downloaded litemap v0.8.1
  Downloaded lazy_static v1.5.0
  Downloaded lazy-regex-proc_macros v3.6.0
  Downloaded itoa v1.0.17
  Downloaded is-docker v0.2.0
  Downloaded ipnet v2.11.0
  Downloaded indexmap v1.9.3
  Downloaded icu_properties v2.1.2
  Downloaded icu_normalizer v2.1.1
  Downloaded iana-time-zone v0.1.65
  Downloaded hyper-tls v0.6.0
  Downloaded html5ever v0.35.0
  Downloaded hex v0.4.3
  Downloaded futures-core v0.3.32
  Downloaded futf v0.1.5
  Downloaded foreign-types v0.3.2
  Downloaded filetime v0.2.27
  Downloaded equivalent v1.0.2
  Downloaded dotenvy v0.15.7
  Downloaded displaydoc v0.2.5
  Downloaded dirs-sys v0.5.0
  Downloaded dialoguer v0.12.0
  Downloaded der-parser v9.0.0
  Downloaded data-encoding v2.10.0
  Downloaded darling v0.23.0
  Downloaded darling v0.21.3
  Downloaded crossbeam-queue v0.3.12
  Downloaded crossbeam v0.8.4
  Downloaded crokey-proc_macros v1.4.0
  Downloaded crokey v1.4.0
  Downloaded colorchoice v1.0.4
  Downloaded block-buffer v0.10.4
  Downloaded bit-vec v0.8.0
  Downloaded axum v0.8.8
  Downloaded aws-lc-rs v1.16.1
  Downloaded async-trait v0.1.89
  Downloaded asn1-rs-impl v0.2.0
  Downloaded asn1-rs-derive v0.5.1
  Downloaded lazy-regex v3.6.0
  Downloaded jobserver v0.1.34
  Downloaded is_terminal_polyfill v1.70.2
  Downloaded is-wsl v0.4.0
  Downloaded idna_adapter v1.2.1
  Downloaded ident_case v1.0.1
  Downloaded icu_provider v2.1.1
  Downloaded hyperlocal v0.9.1
  Downloaded hyper-rustls v0.27.7
  Downloaded http-body v1.0.1
  Downloaded hashbrown v0.16.1
  Downloaded h2 v0.4.13
  Downloaded git2 v0.20.4
  Downloaded futures-io v0.3.32
  Downloaded foldhash v0.2.0
  Downloaded dunce v1.0.5
  Downloaded darling_macro v0.23.0
  Downloaded clap_derive v4.5.55
  Downloaded clap v4.5.60
  Downloaded cfg_aliases v0.2.1
  Downloaded bytecount v0.6.9
  Downloaded borrow-or-share v0.2.4
  Downloaded hashbrown v0.12.3
  Downloaded glob v0.3.3
  Downloaded getrandom v0.4.1
  Downloaded getrandom v0.3.4
  Downloaded crypto-common v0.1.7
  Downloaded coolor v1.1.0
  Downloaded cmake v0.1.57
  Downloaded bytes v1.11.1
  Downloaded atomic-waker v1.1.2
  Downloaded crossbeam-utils v0.8.21
  Downloaded crossbeam-epoch v0.9.18
  Downloaded crossbeam-deque v0.8.6
  Downloaded console v0.16.2
  Downloaded autocfg v1.5.0
  Downloaded asn1-rs v0.6.2
  Downloaded anstream v0.6.21
  Downloaded allocator-api2 v0.2.21
  Downloaded aho-corasick v1.1.4
  Downloaded ahash v0.8.12
  Downloaded anstyle-parse v0.2.7
   Compiling proc-macro2 v1.0.106
   Compiling unicode-ident v1.0.24
   Compiling quote v1.0.44
   Compiling libc v0.2.182
    Checking cfg-if v1.0.4
    Checking once_cell v1.21.3
    Checking smallvec v1.15.1
    Checking log v0.4.29
   Compiling shlex v1.3.0
   Compiling find-msvc-tools v0.1.9
   Compiling syn v2.0.117
   Compiling parking_lot_core v0.9.12
   Compiling jobserver v0.1.34
    Checking memchr v2.8.0
   Compiling cc v1.2.56
    Checking scopeguard v1.2.0
    Checking lock_api v0.4.14
   Compiling serde_core v1.0.228
    Checking parking_lot v0.12.5
    Checking itoa v1.0.17
    Checking pin-project-lite v0.2.17
   Compiling serde v1.0.228
    Checking errno v0.3.14
    Checking signal-hook-registry v1.4.8
    Checking bytes v1.11.1
    Checking mio v1.1.1
    Checking futures-core v0.3.32
   Compiling autocfg v1.5.0
    Checking bitflags v2.11.0
    Checking socket2 v0.6.2
    Checking allocator-api2 v0.2.21
    Checking equivalent v1.0.2
    Checking foldhash v0.2.0
    Checking futures-sink v0.3.32
   Compiling pkg-config v0.3.32
    Checking hashbrown v0.16.1
    Checking tracing-core v0.1.36
    Checking slab v0.4.12
   Compiling vcpkg v0.2.15
    Checking stable_deref_trait v1.2.1
   Compiling synstructure v0.13.2
    Checking indexmap v2.13.0
    Checking futures-channel v0.3.32
    Checking http v1.4.0
    Checking zeroize v1.8.2
   Compiling cmake v0.1.57
    Checking futures-io v0.3.32
    Checking futures-task v0.3.32
   Compiling fs_extra v1.3.0
   Compiling dunce v1.0.5
   Compiling openssl-sys v0.9.111
   Compiling aws-lc-sys v0.38.0
    Checking percent-encoding v2.3.2
    Checking http-body v1.0.1
    Checking rustls-pki-types v1.14.0
   Compiling serde_derive v1.0.228
   Compiling tokio-macros v2.6.0
   Compiling displaydoc v0.2.5
   Compiling zerofrom-derive v0.1.6
    Checking tokio v1.49.0
   Compiling tracing-attributes v0.1.31
    Checking zerofrom v0.1.6
   Compiling yoke-derive v0.8.1
    Checking tracing v0.1.44
   Compiling futures-macro v0.3.32
    Checking yoke v0.8.1
   Compiling zerovec-derive v0.11.2
    Checking futures-util v0.3.32
    Checking zerovec v0.11.5
    Checking getrandom v0.2.17
   Compiling zmij v1.0.21
   Compiling httparse v1.10.1
   Compiling aws-lc-rs v1.16.1
    Checking tinystr v0.8.2
   Compiling ring v0.17.14
    Checking writeable v0.6.2
    Checking base64 v0.22.1
    Checking litemap v0.8.1
    Checking icu_locale_core v2.1.1
    Checking potential_utf v0.1.4
    Checking zerotrie v0.2.3
   Compiling num-traits v0.2.19
    Checking tower-service v0.3.3
   Compiling icu_normalizer_data v2.1.1
   Compiling icu_properties_data v2.1.2
    Checking untrusted v0.7.1
    Checking icu_provider v2.1.1
    Checking icu_collections v2.1.1
    Checking tokio-util v0.7.18
    Checking atomic-waker v1.1.2
    Checking try-lock v0.2.5
    Checking untrusted v0.9.0
    Checking fnv v1.0.7
    Checking want v0.3.1
    Checking h2 v0.4.13
   Compiling serde_json v1.0.149
    Checking httpdate v1.0.3
   Compiling rustls v0.23.37
    Checking pin-utils v0.1.0
    Checking icu_properties v2.1.2
    Checking icu_normalizer v2.1.1
    Checking hyper v1.8.1
    Checking http-body-util v0.1.3
    Checking form_urlencoded v1.2.2
    Checking ipnet v2.11.0
    Checking subtle v2.6.1
    Checking hyper-util v0.1.20
    Checking idna_adapter v1.2.1
    Checking utf8_iter v1.0.4
    Checking openssl-probe v0.2.1
    Checking idna v1.1.0
    Checking sync_wrapper v1.0.2
   Compiling thiserror v2.0.18
    Checking tower-layer v0.3.3
    Checking url v2.5.8
    Checking webpki-roots v1.0.6
   Compiling thiserror-impl v2.0.18
   Compiling openssl v0.10.75
    Checking foreign-types-shared v0.1.1
   Compiling version_check v0.9.5
    Checking foreign-types v0.3.2
    Checking tower v0.5.3
   Compiling openssl-macros v0.1.1
    Checking ryu v1.0.23
   Compiling siphasher v1.0.2
   Compiling native-tls v0.2.18
   Compiling zerocopy v0.8.40
    Checking iri-string v0.7.10
   Compiling ident_case v1.0.1
   Compiling strsim v0.11.1
   Compiling unicase v2.9.0
    Checking mime v0.3.17
   Compiling mime_guess v2.0.5
    Checking tower-http v0.6.8
    Checking serde_urlencoded v0.7.1
   Compiling rustix v1.1.4
    Checking tokio-native-tls v0.3.1
   Compiling signal-hook v0.3.18
    Checking linux-raw-sys v0.12.1
    Checking hyper-tls v0.6.0
    Checking encoding_rs v0.8.35
   Compiling cfg_aliases v0.2.1
   Compiling getrandom v0.3.4
   Compiling rand_core v0.6.4
   Compiling unicode-segmentation v1.12.0
   Compiling convert_case v0.10.0
   Compiling rand v0.8.5
   Compiling phf_shared v0.11.3
    Checking num-integer v0.1.46
    Checking aho-corasick v1.1.4
    Checking regex-syntax v0.8.10
   Compiling crossbeam-utils v0.8.21
   Compiling phf_generator v0.11.3
   Compiling derive_more-impl v2.1.1
    Checking ppv-lite86 v0.2.21
   Compiling libz-sys v1.1.24
   Compiling typenum v1.19.0
    Checking regex-automata v0.4.14
    Checking num-bigint v0.4.6
   Compiling generic-array v0.14.7
   Compiling async-trait v0.1.89
   Compiling anyhow v1.0.102
    Checking new_debug_unreachable v1.0.6
    Checking powerfmt v0.2.0
   Compiling num-conv v0.2.0
    Checking utf-8 v0.7.6
   Compiling getrandom v0.4.1
   Compiling time-core v0.1.8
   Compiling litrs v1.0.0
   Compiling time-macros v0.2.27
   Compiling darling_core v0.21.3
   Compiling document-features v0.2.12
    Checking deranged v0.5.8
   Compiling string_cache_codegen v0.5.4
   Compiling phf_codegen v0.11.3
   Compiling libssh2-sys v0.3.1
    Checking lazy_static v1.5.0
   Compiling ref-cast v1.0.25
   Compiling thiserror v1.0.69
    Checking time v0.3.47
   Compiling darling_macro v0.21.3
   Compiling web_atoms v0.1.3
   Compiling thiserror-impl v1.0.69
   Compiling ref-cast-impl v1.0.25
    Checking mac v0.1.1
    Checking iana-time-zone v0.1.65
    Checking precomputed-hash v0.1.1
    Checking string_cache v0.8.9
    Checking chrono v0.4.44
    Checking futf v0.1.5
    Checking signal-hook-mio v0.2.5
   Compiling darling v0.21.3
    Checking phf v0.11.3
   Compiling toml_datetime v0.6.11
   Compiling serde_spanned v0.6.9
   Compiling derive_more v2.1.1
   Compiling libgit2-sys v0.18.3+1.9.2
   Compiling memoffset v0.9.1
   Compiling toml_write v0.1.2
    Checking unicode-width v0.2.2
   Compiling winnow v0.7.14
   Compiling toml_edit v0.22.27
    Checking crossterm v0.29.0
   Compiling serde_with_macros v3.17.0
   Compiling regex v1.12.3
    Checking tendril v0.4.3
    Checking block-buffer v0.10.4
    Checking crypto-common v0.1.7
    Checking crossbeam-epoch v0.9.18
    Checking crossbeam-channel v0.5.15
   Compiling nix v0.29.0
   Compiling nix v0.31.2
   Compiling darling_core v0.23.0
    Checking futures-executor v0.3.32
   Compiling serde_repr v0.1.20
    Checking data-encoding v2.10.0
    Checking fastrand v2.3.0
   Compiling strict v0.2.0
    Checking utf8parse v0.2.2
    Checking anstyle-parse v0.2.7
   Compiling crokey-proc_macros v1.4.0
    Checking tempfile v3.26.0
    Checking futures v0.3.32
    Checking crossbeam-deque v0.8.6
   Compiling lazy-regex-proc_macros v3.6.0
   Compiling darling_macro v0.23.0
    Checking digest v0.10.7
    Checking markup5ever v0.35.0
    Checking serde_with v3.17.0
   Compiling toml v0.8.23
    Checking sharded-slab v0.1.7
    Checking matchers v0.2.0
    Checking crossbeam-queue v0.3.12
    Checking rand_core v0.9.5
   Compiling phf_shared v0.13.1
   Compiling ahash v0.8.12
   Compiling serde_derive_internals v0.29.1
    Checking tracing-log v0.2.0
    Checking thread_local v1.1.9
    Checking colorchoice v1.0.4
    Checking cpufeatures v0.2.17
    Checking minimal-lexical v0.2.1
    Checking nu-ansi-term v0.50.3
    Checking anstyle v1.0.13
    Checking openssl-probe v0.1.6
    Checking option-ext v0.2.0
    Checking anstyle-query v1.1.5
    Checking is_terminal_polyfill v1.70.2
    Checking dirs-sys v0.5.0
    Checking anstream v0.6.21
    Checking crokey v1.4.0
    Checking tracing-subscriber v0.3.22
    Checking nom v7.1.3
   Compiling schemars_derive v1.2.1
   Compiling phf_generator v0.13.1
    Checking rand_chacha v0.3.1
    Checking rand_chacha v0.9.0
    Checking crossbeam v0.8.4
   Compiling fabro-util v0.4.0 (/home/daytona/workspace/lib/crates/fabro-util)
    Checking lazy-regex v3.6.0
   Compiling darling v0.23.0
    Checking coolor v1.1.0
    Checking console v0.16.2
    Checking num-rational v0.4.2
    Checking num-iter v0.1.45
    Checking rustls-native-certs v0.8.3
    Checking num-complex v0.4.6
    Checking tokio-stream v0.1.18
   Compiling match_token v0.35.0
    Checking minimad v0.14.0
   Compiling heck v0.5.0
   Compiling rmcp v0.15.0
   Compiling unicode-general-category v1.1.0
    Checking clap_lex v1.0.0
    Checking hex v0.4.3
    Checking dyn-clone v1.0.20
    Checking unicode-width v0.1.14
    Checking bit-vec v0.8.0
    Checking borrow-or-share v0.2.4
    Checking bit-set v0.8.0
    Checking fluent-uri v0.4.1
    Checking termimad v0.34.1
    Checking schemars v1.2.1
    Checking clap_builder v4.5.60
   Compiling clap_derive v4.5.55
    Checking html5ever v0.35.0
    Checking num v0.4.3
    Checking process-wrap v9.0.3
   Compiling rmcp-macros v0.15.0
    Checking mac_address v1.1.8
    Checking rand v0.9.2
   Compiling phf_macros v0.13.1
    Checking dirs v6.0.0
    Checking xml5ever v0.35.0
    Checking console v0.15.11
    Checking uuid v1.21.0
    Checking sse-stream v0.2.1
    Checking shell-words v1.1.1
   Compiling pastey v0.2.1
    Checking md5 v0.7.0
    Checking outref v0.5.2
    Checking vsimd v0.8.0
    Checking dialoguer v0.12.0
    Checking uuid-simd v0.8.0
    Checking phf v0.13.1
    Checking markup5ever_rcdom v0.35.0+unofficial
    Checking referencing v0.42.2
    Checking fraction v0.15.3
    Checking clap v4.5.60
    Checking fancy-regex v0.17.0
    Checking hyperlocal v0.9.1
    Checking sha1 v0.10.6
    Checking bollard-stubs v1.47.1-rc.27.3.1
    Checking simple_asn1 v0.6.4
    Checking xattr v1.6.1
    Checking pem v3.0.6
    Checking email_address v0.2.9
    Checking filetime v0.2.27
    Checking signature v2.2.0
    Checking num-cmp v0.1.0
    Checking shell-escape v0.1.5
   Compiling portable-atomic v1.13.1
    Checking bytecount v0.6.9
    Checking tar v0.4.44
    Checking htmd v0.5.0
    Checking rusticata-macros v4.1.0
    Checking fabro-tracker v0.4.0 (/home/daytona/workspace/lib/crates/fabro-tracker)
    Checking webpki-roots v0.26.11
   Compiling asn1-rs-impl v0.2.0
   Compiling asn1-rs-derive v0.5.1
    Checking glob v0.3.3
    Checking same-file v1.0.6
    Checking dotenvy v0.15.7
    Checking unsafe-libyaml v0.2.11
    Checking bollard v0.18.1
    Checking serde_yaml v0.9.34+deprecated
    Checking walkdir v2.5.0
    Checking asn1-rs v0.6.2
    Checking openssh v0.11.6
    Checking sha2 v0.10.9
    Checking is-docker v0.2.0
    Checking unit-prefix v0.5.2
   Compiling oid-registry v0.7.1
    Checking indicatif v0.18.4
    Checking is-wsl v0.4.0
    Checking rustls-webpki v0.103.9
    Checking jsonwebtoken v10.3.0
    Checking ulid v1.2.1
    Checking axum-core v0.5.6
    Checking serde_path_to_error v0.1.20
    Checking pathdiff v0.2.3
    Checking matchit v0.8.4
    Checking open v5.3.3
   Compiling fabro-cli v0.4.0 (/home/daytona/workspace/lib/crates/fabro-cli)
    Checking axum v0.8.8
    Checking der-parser v9.0.0
    Checking x509-parser v0.16.0
    Checking tokio-rustls v0.26.4
    Checking hyper-rustls v0.27.7
    Checking reqwest v0.12.28
    Checking rustls-platform-verifier v0.6.2
    Checking reqwest v0.13.2
    Checking reqwest-middleware v0.4.2
    Checking jsonschema v0.42.2
    Checking tungstenite v0.26.2
    Checking tokio-tungstenite v0.26.2
    Checking daytona-toolbox-client v0.1.0 (https://github.com/brynary/daytona-sdk-rust?rev=06033ca#06033caa)
    Checking daytona-api-client v0.1.0 (https://github.com/brynary/daytona-sdk-rust?rev=06033ca#06033caa)
    Checking fabro-github v0.4.0 (/home/daytona/workspace/lib/crates/fabro-github)
    Checking fabro-devcontainer v0.4.0 (/home/daytona/workspace/lib/crates/fabro-devcontainer)
    Checking fabro-openai-oauth v0.4.0 (/home/daytona/workspace/lib/crates/fabro-openai-oauth)
    Checking tracing-appender v0.2.4
    Checking rustls-pemfile v2.2.0
    Checking semver v1.0.27
    Checking fabro-mcp v0.4.0 (/home/daytona/workspace/lib/crates/fabro-mcp)
    Checking git2 v0.20.4
    Checking fabro-git-storage v0.4.0 (/home/daytona/workspace/lib/crates/fabro-git-storage)
    Checking daytona-sdk v0.1.0 (https://github.com/brynary/daytona-sdk-rust?rev=06033ca#06033caa)
    Checking fabro-llm v0.4.0 (/home/daytona/workspace/lib/crates/fabro-llm)
    Checking fabro-agent v0.4.0 (/home/daytona/workspace/lib/crates/fabro-agent)
    Checking fabro-ssh v0.4.0 (/home/daytona/workspace/lib/crates/fabro-ssh)
    Checking fabro-workflows v0.4.0 (/home/daytona/workspace/lib/crates/fabro-workflows)
    Checking fabro-config v0.4.0 (/home/daytona/workspace/lib/crates/fabro-config)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 12s
    ```
  - Stderr: (empty)
- **preflight_lint**: success
  - Script: `cargo clippy -- -D warnings 2>&1`
  - Stdout:
    ```
    Compiling fabro-util v0.4.0 (/home/daytona/workspace/lib/crates/fabro-util)
    Checking fabro-mcp v0.4.0 (/home/daytona/workspace/lib/crates/fabro-mcp)
    Checking fabro-tracker v0.4.0 (/home/daytona/workspace/lib/crates/fabro-tracker)
    Checking fabro-git-storage v0.4.0 (/home/daytona/workspace/lib/crates/fabro-git-storage)
    Checking fabro-github v0.4.0 (/home/daytona/workspace/lib/crates/fabro-github)
    Checking fabro-devcontainer v0.4.0 (/home/daytona/workspace/lib/crates/fabro-devcontainer)
   Compiling fabro-cli v0.4.0 (/home/daytona/workspace/lib/crates/fabro-cli)
    Checking fabro-openai-oauth v0.4.0 (/home/daytona/workspace/lib/crates/fabro-openai-oauth)
    Checking fabro-llm v0.4.0 (/home/daytona/workspace/lib/crates/fabro-llm)
    Checking fabro-agent v0.4.0 (/home/daytona/workspace/lib/crates/fabro-agent)
    Checking fabro-ssh v0.4.0 (/home/daytona/workspace/lib/crates/fabro-ssh)
    Checking fabro-workflows v0.4.0 (/home/daytona/workspace/lib/crates/fabro-workflows)
    Checking fabro-config v0.4.0 (/home/daytona/workspace/lib/crates/fabro-config)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 18.35s
    ```
  - Stderr: (empty)


Read the plan file referenced in the goal and implement every step. Make all the code changes described in the plan.