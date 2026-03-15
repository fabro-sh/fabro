Goal: # Plan: `fabro asset` CLI subcommands

## Context

After a workflow run, assets (screenshots, test reports, traces) are collected to `~/.fabro/runs/{id}/artifacts/assets/` but there's no CLI for inspecting them. Users must browse the filesystem manually or read paths printed at run completion. We want dedicated subcommands for listing and copying assets.

## Proposed CLI surface

```
fabro asset list <RUN_ID>                    # list all assets for a run
fabro asset cp <RUN_ID> [DEST]               # copy all assets to DEST (default: cwd)
fabro asset cp <RUN_ID>:<PATH> [DEST]        # copy a specific asset
```

### `fabro asset list <RUN_ID>`

**Args:** `run_id` (positional), `--node <slug>` (filter), `--json`

**Default output:**
```
NODE        RETRY  SIZE       PATH
run_tests   1      8.2 KB     test-results/report.xml
run_tests   1      3.1 KB     test-results/screenshot.png
lint        1      2.1 KB     coverage/lcov.info

3 asset(s), 13.4 KB total
```

**JSON output:** array of `{ node, retry, path, size, absolute_path }`

### `fabro asset cp <RUN_ID>[:<PATH>] [DEST]`

**Args:**
- `source` (positional) — either `RUN_ID` (all assets) or `RUN_ID:path` (specific asset)
- `dest` (positional, optional) — destination directory, defaults to `.` (cwd)
- `--node <slug>` — filter to a specific node (for bulk) or disambiguate (for specific path)
- `--tree` — preserve `{node_slug}/retry_{N}/` directory structure (default is flat)

**Behavior:**
- `fabro asset cp RUN_ID` — copies all assets flat into cwd
- `fabro asset cp RUN_ID ./out` — copies all assets flat into `./out`
- `fabro asset cp RUN_ID:report.xml` — copies that specific file to cwd
- `fabro asset cp RUN_ID --tree ./out` — copies all assets preserving `{node}/retry_{N}/` structure
- Flat mode errors on filename collision across nodes (suggests `--tree` or `--node`)
- Uses local asset store (not sandbox). Distinct from `fabro cp` which talks to the live sandbox.

**Parsing `source`:** Reuse the same `split on first colon` logic from `cp.rs:split_run_path()`. If no colon, it's a bare run ID. If colon present, everything after is the asset path.

## Implementation

### New file: `lib/crates/fabro-workflows/src/cli/asset.rs`

Core struct and scanning function:

```rust
pub struct AssetEntry {
    pub node_slug: String,
    pub retry: u32,
    pub relative_path: String,
    pub absolute_path: PathBuf,
    pub size: u64,
}

pub fn scan_assets(run_dir: &Path, node_filter: Option<&str>) -> Result<Vec<AssetEntry>>
```

`scan_assets` walks `{run_dir}/artifacts/assets/*/retry_*/manifest.json`, reads each `AssetCollectionSummary`, stats each file on disk, returns structured entries. Similar to existing `collect_asset_paths()` but returns richer data.

Two handler functions: `list_command`, `cp_command`.

### Modified files

1. **`lib/crates/fabro-workflows/src/cli/mod.rs`** — add `pub mod asset;`

2. **`lib/crates/fabro-cli/src/main.rs`**:
   - Add `AssetCommand` enum (List, Cp variants)
   - Add `Asset { command: AssetCommand }` to `Command` enum
   - Add `command_name` match arms (`"asset list"`, `"asset cp"`)
   - Add `main_inner` match arms dispatching to handler functions

3. **`lib/crates/fabro-workflows/src/cli/runs.rs`** — make `format_size` `pub(crate)`

### Reuse

- `runs::default_runs_base()` / `runs::find_run_by_prefix()` — run resolution
- `asset_snapshot::AssetCollectionSummary` — manifest deserialization
- `runs::format_size()` — human-readable byte formatting (change from `fn` to `pub(crate) fn`)
- `cli::tilde_path()` — path display with `~` shortening
- `cp::split_run_path()` pattern — parsing `RUN_ID:path` syntax

## Verification

1. `cargo build -p fabro-cli` — compiles
2. `cargo test -p fabro-workflows` — existing tests pass
3. `fabro asset list <recent-run-id>` — shows table
4. `fabro asset list <recent-run-id> --json` — shows JSON
5. `fabro asset cp <recent-run-id>` — copies all assets to cwd
6. `fabro asset cp <recent-run-id>:test-results/report.xml` — copies specific file to cwd
7. `fabro asset cp <recent-run-id> --tree ./out` — copies with directory structure


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
  Downloaded anstyle-parse v0.2.7
  Downloaded axum-core v0.5.6
  Downloaded futures-core v0.3.32
  Downloaded futures-io v0.3.32
  Downloaded http-body v1.0.1
  Downloaded foreign-types v0.3.2
  Downloaded dotenvy v0.15.7
  Downloaded powerfmt v0.2.0
  Downloaded precomputed-hash v0.1.1
  Downloaded rand_chacha v0.3.1
  Downloaded futures v0.3.32
  Downloaded rand_core v0.6.4
  Downloaded rustls-pki-types v1.14.0
  Downloaded rustls-native-certs v0.8.3
  Downloaded serde_path_to_error v0.1.20
  Downloaded stable_deref_trait v1.2.1
  Downloaded socket2 v0.6.2
  Downloaded time-macros v0.2.27
  Downloaded untrusted v0.9.0
  Downloaded uuid-simd v0.8.0
  Downloaded webpki-roots v0.26.11
  Downloaded libz-sys v1.1.24
  Downloaded yoke v0.8.1
  Downloaded zmij v1.0.21
  Downloaded winnow v0.7.14
  Downloaded want v0.3.1
  Downloaded zerovec-derive v0.11.2
  Downloaded webpki-roots v1.0.6
  Downloaded tinystr v0.8.2
  Downloaded yoke-derive v0.8.1
  Downloaded version_check v0.9.5
  Downloaded zerotrie v0.2.3
  Downloaded pastey v0.2.1
  Downloaded zerofrom v0.1.6
  Downloaded rand_chacha v0.9.0
  Downloaded foreign-types-shared v0.1.1
  Downloaded phf_generator v0.13.1
  Downloaded fluent-uri v0.4.1
  Downloaded zerofrom-derive v0.1.6
  Downloaded signal-hook-registry v1.4.8
  Downloaded semver v1.0.27
  Downloaded syn v2.0.117
  Downloaded unicode-segmentation v1.12.0
  Downloaded writeable v0.6.2
  Downloaded walkdir v2.5.0
  Downloaded toml v0.8.23
  Downloaded bitflags v2.11.0
  Downloaded serde_derive_internals v0.29.1
  Downloaded aws-lc-rs v1.16.1
  Downloaded utf8parse v0.2.2
  Downloaded idna v1.1.0
  Downloaded asn1-rs-derive v0.5.1
  Downloaded same-file v1.0.6
  Downloaded unicode-ident v1.0.24
  Downloaded aws-lc-sys v0.38.0
  Downloaded shell-escape v0.1.5
  Downloaded tokio-util v0.7.18
  Downloaded toml_write v0.1.2
  Downloaded console v0.15.11
  Downloaded serde_urlencoded v0.7.1
  Downloaded generic-array v0.14.7
  Downloaded vcpkg v0.2.15
  Downloaded url v2.5.8
  Downloaded tracing-attributes v0.1.31
  Downloaded tinyvec_macros v0.1.1
  Downloaded smallvec v1.15.1
  Downloaded tracing-subscriber v0.3.22
  Downloaded termimad v0.34.1
  Downloaded rustls v0.23.37
  Downloaded der-parser v9.0.0
  Downloaded ref-cast-impl v1.0.25
  Downloaded schemars v0.9.0
  Downloaded ring v0.17.14
  Downloaded linux-raw-sys v0.12.1
  Downloaded proc-macro2 v1.0.106
  Downloaded toml_edit v0.22.27
  Downloaded pkg-config v0.3.32
  Downloaded data-encoding v2.10.0
  Downloaded zeroize v1.8.2
  Downloaded xml5ever v0.35.0
  Downloaded web_atoms v0.1.3
  Downloaded uuid v1.21.0
  Downloaded untrusted v0.7.1
  Downloaded tungstenite v0.26.2
  Downloaded tracing-core v0.1.36
  Downloaded tokio-tungstenite v0.26.2
  Downloaded thiserror-impl v1.0.69
  Downloaded zerocopy v0.8.40
  Downloaded futf v0.1.5
  Downloaded utf8_iter v1.0.4
  Downloaded ulid v1.2.1
  Downloaded tower-layer v0.3.3
  Downloaded thiserror v1.0.69
  Downloaded strict v0.2.0
  Downloaded signature v2.2.0
  Downloaded phf_generator v0.11.3
  Downloaded fraction v0.15.3
  Downloaded console v0.16.2
  Downloaded cc v1.2.56
  Downloaded bytecount v0.6.9
  Downloaded unicode-general-category v1.1.0
  Downloaded tokio-rustls v0.26.4
  Downloaded time-core v0.1.8
  Downloaded tempfile v3.26.0
  Downloaded synstructure v0.13.2
  Downloaded serde_with_macros v3.17.0
  Downloaded serde_repr v0.1.20
  Downloaded ryu v1.0.23
  Downloaded regex v1.12.3
  Downloaded quote v1.0.44
  Downloaded fancy-regex v0.17.0
  Downloaded derive_more-impl v2.1.1
  Downloaded termcolor v1.4.1
  Downloaded strsim v0.11.1
  Downloaded sharded-slab v0.1.7
  Downloaded rustls-pemfile v2.2.0
  Downloaded potential_utf v0.1.4
  Downloaded nix v0.29.0
  Downloaded libc v0.2.182
  Downloaded futures-macro v0.3.32
  Downloaded futures-executor v0.3.32
  Downloaded futures-channel v0.3.32
  Downloaded equivalent v1.0.2
  Downloaded tracing v0.1.44
  Downloaded tower v0.5.3
  Downloaded siphasher v1.0.2
  Downloaded signal-hook-mio v0.2.5
  Downloaded shlex v1.3.0
  Downloaded sha1 v0.10.6
  Downloaded reqwest v0.13.2
  Downloaded ppv-lite86 v0.2.21
  Downloaded libssh2-sys v0.3.1
  Downloaded dunce v1.0.5
  Downloaded digest v0.10.7
  Downloaded unit-prefix v0.5.2
  Downloaded tokio-macros v2.6.0
  Downloaded thread_local v1.1.9
  Downloaded rustc_version v0.4.1
  Downloaded quinn-proto v0.11.14
  Downloaded phf_shared v0.11.3
  Downloaded icu_normalizer_data v2.1.1
  Downloaded darling v0.21.3
  Downloaded bit-vec v0.8.0
  Downloaded tokio v1.49.0
  Downloaded time v0.3.47
  Downloaded rustix v1.1.4
  Downloaded rmcp-macros v0.15.0
  Downloaded pem v3.0.6
  Downloaded markup5ever_rcdom v0.35.0+unofficial
  Downloaded icu_properties_data v2.1.2
  Downloaded try-lock v0.2.5
  Downloaded tracing-log v0.2.0
  Downloaded subtle v2.6.1
  Downloaded scopeguard v1.2.0
  Downloaded regex-syntax v0.8.10
  Downloaded openssl-sys v0.9.111
  Downloaded libgit2-sys v0.18.3+1.9.2
  Downloaded indexmap v2.13.0
  Downloaded icu_locale_core v2.1.1
  Downloaded icu_collections v2.1.1
  Downloaded zerovec v0.11.5
  Downloaded x509-parser v0.16.0
  Downloaded vsimd v0.8.0
  Downloaded utf-8 v0.7.6
  Downloaded unicode-width v0.2.2
  Downloaded toml_datetime v0.6.11
  Downloaded sse-stream v0.2.1
  Downloaded serde_spanned v0.6.9
  Downloaded serde_core v1.0.228
  Downloaded rustls-platform-verifier v0.6.2
  Downloaded rusticata-macros v4.1.0
  Downloaded referencing v0.42.2
  Downloaded rand_core v0.9.5
  Downloaded quinn v0.11.9
  Downloaded phf v0.11.3
  Downloaded num-complex v0.4.6
  Downloaded mime_guess v2.0.5
  Downloaded memoffset v0.9.1
  Downloaded log v0.4.29
  Downloaded litrs v1.0.0
  Downloaded lazy-regex-proc_macros v3.6.0
  Downloaded jsonwebtoken v10.3.0
  Downloaded is-docker v0.2.0
  Downloaded ipnet v2.11.0
  Downloaded futures-task v0.3.32
  Downloaded foldhash v0.2.0
  Downloaded filetime v0.2.27
  Downloaded email_address v0.2.9
  Downloaded document-features v0.2.12
  Downloaded dirs-sys v0.5.0
  Downloaded darling_macro v0.21.3
  Downloaded crokey v1.4.0
  Downloaded xattr v1.6.1
  Downloaded tracing-appender v0.2.4
  Downloaded tinyvec v1.10.0
  Downloaded phf_codegen v0.11.3
  Downloaded is_terminal_polyfill v1.70.2
  Downloaded idna_adapter v1.2.1
  Downloaded unicode-width v0.1.14
  Downloaded unicase v2.9.0
  Downloaded typenum v1.19.0
  Downloaded num-conv v0.2.0
  Downloaded minimal-lexical v0.2.1
  Downloaded jobserver v0.1.34
  Downloaded tower-http v0.6.8
  Downloaded string_cache v0.8.9
  Downloaded rand v0.9.2
  Downloaded ident_case v1.0.1
  Downloaded unsafe-libyaml v0.2.11
  Downloaded tower-service v0.3.3
  Downloaded tokio-stream v0.1.18
  Downloaded tokio-native-tls v0.3.1
  Downloaded tendril v0.4.3
  Downloaded tar v0.4.44
  Downloaded sync_wrapper v1.0.2
  Downloaded string_cache_codegen v0.5.4
  Downloaded simple_asn1 v0.6.4
  Downloaded serde_yaml v0.9.34+deprecated
  Downloaded rmcp v0.15.0
  Downloaded pin-project-lite v0.2.17
  Downloaded nix v0.31.2
  Downloaded htmd v0.5.0
  Downloaded thiserror-impl v2.0.18
  Downloaded thiserror v2.0.18
  Downloaded serde_with v3.17.0
  Downloaded serde_json v1.0.149
  Downloaded percent-encoding v2.3.2
  Downloaded openssl v0.10.75
  Downloaded openssh v0.11.6
  Downloaded nu-ansi-term v0.50.3
  Downloaded crossbeam-channel v0.5.15
  Downloaded bytes v1.11.1
  Downloaded autocfg v1.5.0
  Downloaded slab v0.4.12
  Downloaded signal-hook v0.3.18
  Downloaded regex-automata v0.4.14
  Downloaded phf v0.13.1
  Downloaded git2 v0.20.4
  Downloaded shell-words v1.1.1
  Downloaded sha2 v0.10.9
  Downloaded openssl-probe v0.2.1
  Downloaded num-traits v0.2.19
  Downloaded jsonschema v0.42.2
  Downloaded icu_provider v2.1.1
  Downloaded hyper v1.8.1
  Downloaded find-msvc-tools v0.1.9
  Downloaded encoding_rs v0.8.35
  Downloaded dirs v6.0.0
  Downloaded dialoguer v0.12.0
  Downloaded serde_derive v1.0.228
  Downloaded serde v1.0.228
  Downloaded schemars_derive v1.2.1
  Downloaded schemars v1.2.1
  Downloaded rustls-webpki v0.103.9
  Downloaded rustc-hash v2.1.1
  Downloaded reqwest v0.12.28
  Downloaded process-wrap v9.0.3
  Downloaded portable-atomic v1.13.1
  Downloaded parking_lot v0.12.5
  Downloaded outref v0.5.2
  Downloaded num-iter v0.1.45
  Downloaded num v0.4.3
  Downloaded nom v7.1.3
  Downloaded native-tls v0.2.18
  Downloaded matchers v0.2.0
  Downloaded mac v0.1.1
  Downloaded lazy-regex v3.6.0
  Downloaded iri-string v0.7.10
  Downloaded h2 v0.4.13
  Downloaded futures-sink v0.3.32
  Downloaded crossbeam v0.8.4
  Downloaded bollard v0.18.1
  Downloaded reqwest-middleware v0.4.2
  Downloaded is-wsl v0.4.0
  Downloaded hyper-util v0.1.20
  Downloaded hashbrown v0.16.1
  Downloaded crossterm v0.29.0
  Downloaded coolor v1.1.0
  Downloaded clap_derive v4.5.55
  Downloaded ref-cast v1.0.25
  Downloaded rand v0.8.5
  Downloaded quinn-udp v0.5.14
  Downloaded openssl-probe v0.1.6
  Downloaded open v5.3.3
  Downloaded once_cell v1.21.3
  Downloaded num-cmp v0.1.0
  Downloaded num-bigint v0.4.6
  Downloaded mio v1.1.1
  Downloaded mime v0.3.17
  Downloaded memchr v2.8.0
  Downloaded matchit v0.8.4
  Downloaded indicatif v0.18.4
  Downloaded indexmap v1.9.3
  Downloaded icu_properties v2.1.2
  Downloaded icu_normalizer v2.1.1
  Downloaded iana-time-zone v0.1.65
  Downloaded httparse v1.10.1
  Downloaded http v1.4.0
  Downloaded html5ever v0.35.0
  Downloaded hashbrown v0.12.3
  Downloaded getrandom v0.4.1
  Downloaded getrandom v0.3.4
  Downloaded displaydoc v0.2.5
  Downloaded deranged v0.5.8
  Downloaded cli-table v0.5.0
  Downloaded option-ext v0.2.0
  Downloaded openssl-macros v0.1.1
  Downloaded oid-registry v0.7.1
  Downloaded num-rational v0.4.2
  Downloaded num-integer v0.1.46
  Downloaded new_debug_unreachable v1.0.6
  Downloaded minimad v0.14.0
  Downloaded md5 v0.7.0
  Downloaded match_token v0.35.0
  Downloaded markup5ever v0.35.0
  Downloaded mac_address v1.1.8
  Downloaded lock_api v0.4.14
  Downloaded litemap v0.8.1
  Downloaded hyper-rustls v0.27.7
  Downloaded getrandom v0.2.17
  Downloaded colorchoice v1.0.4
  Downloaded bollard-stubs v1.47.1-rc.27.3.1
  Downloaded phf_shared v0.13.1
  Downloaded phf_macros v0.13.1
  Downloaded heck v0.5.0
  Downloaded fs_extra v1.3.0
  Downloaded cfg-if v1.0.4
  Downloaded pathdiff v0.2.3
  Downloaded parking_lot_core v0.9.12
  Downloaded lru-slab v0.1.2
  Downloaded glob v0.3.3
  Downloaded futures-util v0.3.32
  Downloaded darling_core v0.21.3
  Downloaded crossbeam-queue v0.3.12
  Downloaded crossbeam-epoch v0.9.18
  Downloaded clap_builder v4.5.60
  Downloaded clap v4.5.60
  Downloaded chrono v0.4.44
  Downloaded cfg_aliases v0.2.1
  Downloaded aho-corasick v1.1.4
  Downloaded pin-utils v0.1.0
  Downloaded lazy_static v1.5.0
  Downloaded itoa v1.0.17
  Downloaded hyperlocal v0.9.1
  Downloaded hyper-tls v0.6.0
  Downloaded httpdate v1.0.3
  Downloaded http-body-util v0.1.3
  Downloaded hex v0.4.3
  Downloaded derive_more v2.1.1
  Downloaded darling_core v0.23.0
  Downloaded crossbeam-deque v0.8.6
  Downloaded axum v0.8.8
  Downloaded asn1-rs v0.6.2
  Downloaded darling v0.23.0
  Downloaded crossbeam-utils v0.8.21
  Downloaded base64 v0.22.1
  Downloaded cmake v0.1.57
  Downloaded bit-set v0.8.0
  Downloaded asn1-rs-impl v0.2.0
  Downloaded cpufeatures v0.2.17
  Downloaded convert_case v0.10.0
  Downloaded form_urlencoded v1.2.2
  Downloaded fastrand v2.3.0
  Downloaded crypto-common v0.1.7
  Downloaded crokey-proc_macros v1.4.0
  Downloaded borrow-or-share v0.2.4
  Downloaded fnv v1.0.7
  Downloaded errno v0.3.14
  Downloaded dyn-clone v1.0.20
  Downloaded darling_macro v0.23.0
  Downloaded clap_lex v1.0.0
  Downloaded block-buffer v0.10.4
  Downloaded async-trait v0.1.89
  Downloaded anyhow v1.0.102
  Downloaded anstyle-query v1.1.5
  Downloaded anstyle v1.0.13
  Downloaded allocator-api2 v0.2.21
  Downloaded ahash v0.8.12
  Downloaded atomic-waker v1.1.2
  Downloaded anstream v0.6.21
   Compiling proc-macro2 v1.0.106
   Compiling quote v1.0.44
   Compiling unicode-ident v1.0.24
   Compiling libc v0.2.182
    Checking cfg-if v1.0.4
    Checking once_cell v1.21.3
    Checking smallvec v1.15.1
    Checking log v0.4.29
   Compiling shlex v1.3.0
   Compiling find-msvc-tools v0.1.9
   Compiling syn v2.0.117
   Compiling jobserver v0.1.34
   Compiling parking_lot_core v0.9.12
   Compiling cc v1.2.56
    Checking memchr v2.8.0
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
    Checking foldhash v0.2.0
    Checking allocator-api2 v0.2.21
    Checking equivalent v1.0.2
    Checking futures-sink v0.3.32
   Compiling pkg-config v0.3.32
    Checking tracing-core v0.1.36
    Checking hashbrown v0.16.1
    Checking slab v0.4.12
   Compiling synstructure v0.13.2
    Checking stable_deref_trait v1.2.1
   Compiling vcpkg v0.2.15
    Checking indexmap v2.13.0
    Checking futures-channel v0.3.32
    Checking http v1.4.0
    Checking zeroize v1.8.2
   Compiling cmake v0.1.57
   Compiling dunce v1.0.5
    Checking futures-task v0.3.32
    Checking futures-io v0.3.32
   Compiling fs_extra v1.3.0
   Compiling openssl-sys v0.9.111
    Checking percent-encoding v2.3.2
    Checking http-body v1.0.1
   Compiling aws-lc-sys v0.38.0
    Checking rustls-pki-types v1.14.0
    Checking getrandom v0.2.17
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
   Compiling zerovec-derive v0.11.2
    Checking yoke v0.8.1
    Checking futures-util v0.3.32
    Checking zerovec v0.11.5
   Compiling aws-lc-rs v1.16.1
   Compiling httparse v1.10.1
   Compiling zmij v1.0.21
    Checking tinystr v0.8.2
   Compiling ring v0.17.14
    Checking base64 v0.22.1
    Checking litemap v0.8.1
    Checking writeable v0.6.2
    Checking icu_locale_core v2.1.1
    Checking potential_utf v0.1.4
    Checking zerotrie v0.2.3
   Compiling num-traits v0.2.19
   Compiling icu_normalizer_data v2.1.1
    Checking tower-service v0.3.3
    Checking untrusted v0.7.1
   Compiling icu_properties_data v2.1.2
    Checking icu_provider v2.1.1
    Checking icu_collections v2.1.1
    Checking tokio-util v0.7.18
    Checking untrusted v0.9.0
    Checking try-lock v0.2.5
    Checking fnv v1.0.7
    Checking atomic-waker v1.1.2
    Checking want v0.3.1
    Checking h2 v0.4.13
   Compiling rustls v0.23.37
   Compiling serde_json v1.0.149
    Checking httpdate v1.0.3
    Checking pin-utils v0.1.0
    Checking icu_normalizer v2.1.1
    Checking icu_properties v2.1.2
    Checking hyper v1.8.1
    Checking http-body-util v0.1.3
    Checking form_urlencoded v1.2.2
    Checking subtle v2.6.1
    Checking ipnet v2.11.0
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
   Compiling zerocopy v0.8.40
   Compiling native-tls v0.2.18
   Compiling siphasher v1.0.2
    Checking mime v0.3.17
    Checking iri-string v0.7.10
   Compiling unicase v2.9.0
   Compiling strsim v0.11.1
   Compiling ident_case v1.0.1
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
   Compiling unicode-segmentation v1.12.0
   Compiling getrandom v0.3.4
   Compiling rand_core v0.6.4
   Compiling rand v0.8.5
   Compiling convert_case v0.10.0
   Compiling phf_shared v0.11.3
    Checking num-integer v0.1.46
    Checking aho-corasick v1.1.4
   Compiling crossbeam-utils v0.8.21
    Checking regex-syntax v0.8.10
   Compiling phf_generator v0.11.3
   Compiling derive_more-impl v2.1.1
    Checking ppv-lite86 v0.2.21
   Compiling libz-sys v1.1.24
   Compiling typenum v1.19.0
    Checking regex-automata v0.4.14
    Checking num-bigint v0.4.6
   Compiling generic-array v0.14.7
   Compiling async-trait v0.1.89
    Checking new_debug_unreachable v1.0.6
   Compiling getrandom v0.4.1
    Checking powerfmt v0.2.0
   Compiling anyhow v1.0.102
   Compiling litrs v1.0.0
   Compiling time-core v0.1.8
   Compiling num-conv v0.2.0
    Checking utf-8 v0.7.6
   Compiling time-macros v0.2.27
    Checking deranged v0.5.8
   Compiling document-features v0.2.12
   Compiling darling_core v0.21.3
   Compiling string_cache_codegen v0.5.4
   Compiling phf_codegen v0.11.3
   Compiling libssh2-sys v0.3.1
   Compiling thiserror v1.0.69
   Compiling ref-cast v1.0.25
    Checking lazy_static v1.5.0
    Checking time v0.3.47
   Compiling darling_macro v0.21.3
   Compiling web_atoms v0.1.3
   Compiling ref-cast-impl v1.0.25
   Compiling thiserror-impl v1.0.69
    Checking iana-time-zone v0.1.65
    Checking mac v0.1.1
    Checking unicode-width v0.2.2
    Checking precomputed-hash v0.1.1
    Checking string_cache v0.8.9
    Checking chrono v0.4.44
    Checking futf v0.1.5
    Checking signal-hook-mio v0.2.5
   Compiling darling v0.21.3
    Checking phf v0.11.3
   Compiling serde_spanned v0.6.9
   Compiling toml_datetime v0.6.11
   Compiling derive_more v2.1.1
   Compiling libgit2-sys v0.18.3+1.9.2
   Compiling memoffset v0.9.1
   Compiling winnow v0.7.14
   Compiling toml_write v0.1.2
   Compiling toml_edit v0.22.27
    Checking crossterm v0.29.0
   Compiling serde_with_macros v3.17.0
    Checking tendril v0.4.3
   Compiling regex v1.12.3
    Checking block-buffer v0.10.4
    Checking crypto-common v0.1.7
    Checking crossbeam-channel v0.5.15
    Checking crossbeam-epoch v0.9.18
   Compiling nix v0.31.2
   Compiling nix v0.29.0
   Compiling darling_core v0.23.0
    Checking futures-executor v0.3.32
   Compiling serde_repr v0.1.20
    Checking fastrand v2.3.0
    Checking utf8parse v0.2.2
   Compiling strict v0.2.0
    Checking data-encoding v2.10.0
   Compiling crokey-proc_macros v1.4.0
    Checking anstyle-parse v0.2.7
    Checking tempfile v3.26.0
    Checking futures v0.3.32
   Compiling lazy-regex-proc_macros v3.6.0
   Compiling darling_macro v0.23.0
    Checking crossbeam-deque v0.8.6
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
    Checking openssl-probe v0.1.6
    Checking option-ext v0.2.0
    Checking nu-ansi-term v0.50.3
    Checking minimal-lexical v0.2.1
    Checking anstyle-query v1.1.5
    Checking cpufeatures v0.2.17
    Checking anstyle v1.0.13
    Checking colorchoice v1.0.4
    Checking is_terminal_polyfill v1.70.2
    Checking anstream v0.6.21
    Checking crokey v1.4.0
   Compiling phf_generator v0.13.1
   Compiling schemars_derive v1.2.1
    Checking nom v7.1.3
    Checking tracing-subscriber v0.3.22
    Checking dirs-sys v0.5.0
    Checking rand_chacha v0.3.1
    Checking rand_chacha v0.9.0
    Checking crossbeam v0.8.4
   Compiling fabro-util v0.5.0 (/home/daytona/workspace/lib/crates/fabro-util)
   Compiling darling v0.23.0
    Checking lazy-regex v3.6.0
    Checking coolor v1.1.0
    Checking console v0.16.2
    Checking num-rational v0.4.2
    Checking num-iter v0.1.45
    Checking rustls-native-certs v0.8.3
    Checking num-complex v0.4.6
    Checking tokio-stream v0.1.18
   Compiling match_token v0.35.0
    Checking minimad v0.14.0
    Checking unicode-width v0.1.14
   Compiling rmcp v0.15.0
    Checking dyn-clone v1.0.20
   Compiling unicode-general-category v1.1.0
   Compiling heck v0.5.0
    Checking hex v0.4.3
    Checking bit-vec v0.8.0
    Checking borrow-or-share v0.2.4
    Checking clap_lex v1.0.0
    Checking fluent-uri v0.4.1
    Checking clap_builder v4.5.60
    Checking bit-set v0.8.0
   Compiling clap_derive v4.5.55
    Checking schemars v1.2.1
    Checking termimad v0.34.1
    Checking html5ever v0.35.0
    Checking num v0.4.3
    Checking process-wrap v9.0.3
   Compiling rmcp-macros v0.15.0
    Checking mac_address v1.1.8
    Checking rand v0.9.2
    Checking dirs v6.0.0
   Compiling phf_macros v0.13.1
    Checking xml5ever v0.35.0
    Checking console v0.15.11
    Checking uuid v1.21.0
    Checking sse-stream v0.2.1
    Checking md5 v0.7.0
    Checking termcolor v1.4.1
   Compiling pastey v0.2.1
    Checking shell-words v1.1.1
    Checking outref v0.5.2
    Checking vsimd v0.8.0
    Checking dialoguer v0.12.0
    Checking uuid-simd v0.8.0
    Checking cli-table v0.5.0
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
   Compiling portable-atomic v1.13.1
    Checking num-cmp v0.1.0
    Checking bytecount v0.6.9
    Checking signature v2.2.0
    Checking shell-escape v0.1.5
    Checking tar v0.4.44
    Checking htmd v0.5.0
    Checking rusticata-macros v4.1.0
    Checking fabro-tracker v0.5.0 (/home/daytona/workspace/lib/crates/fabro-tracker)
    Checking webpki-roots v0.26.11
   Compiling asn1-rs-derive v0.5.1
   Compiling asn1-rs-impl v0.2.0
    Checking glob v0.3.3
    Checking same-file v1.0.6
    Checking unsafe-libyaml v0.2.11
    Checking dotenvy v0.15.7
    Checking serde_yaml v0.9.34+deprecated
    Checking bollard v0.18.1
    Checking walkdir v2.5.0
    Checking asn1-rs v0.6.2
    Checking openssh v0.11.6
    Checking sha2 v0.10.9
    Checking is-docker v0.2.0
    Checking unit-prefix v0.5.2
   Compiling oid-registry v0.7.1
    Checking indicatif v0.18.4
    Checking rustls-webpki v0.103.9
    Checking jsonwebtoken v10.3.0
    Checking is-wsl v0.4.0
    Checking ulid v1.2.1
    Checking axum-core v0.5.6
    Checking serde_path_to_error v0.1.20
    Checking pathdiff v0.2.3
    Checking matchit v0.8.4
    Checking open v5.3.3
   Compiling fabro-cli v0.5.0 (/home/daytona/workspace/lib/crates/fabro-cli)
    Checking der-parser v9.0.0
    Checking axum v0.8.8
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
    Checking daytona-api-client v0.1.0 (https://github.com/brynary/daytona-sdk-rust?rev=06033ca#06033caa)
    Checking daytona-toolbox-client v0.1.0 (https://github.com/brynary/daytona-sdk-rust?rev=06033ca#06033caa)
    Checking fabro-github v0.5.0 (/home/daytona/workspace/lib/crates/fabro-github)
    Checking fabro-devcontainer v0.5.0 (/home/daytona/workspace/lib/crates/fabro-devcontainer)
    Checking fabro-openai-oauth v0.5.0 (/home/daytona/workspace/lib/crates/fabro-openai-oauth)
    Checking tracing-appender v0.2.4
    Checking git2 v0.20.4
    Checking rustls-pemfile v2.2.0
    Checking semver v1.0.27
    Checking fabro-mcp v0.5.0 (/home/daytona/workspace/lib/crates/fabro-mcp)
    Checking fabro-git-storage v0.5.0 (/home/daytona/workspace/lib/crates/fabro-git-storage)
    Checking fabro-llm v0.5.0 (/home/daytona/workspace/lib/crates/fabro-llm)
    Checking daytona-sdk v0.1.0 (https://github.com/brynary/daytona-sdk-rust?rev=06033ca#06033caa)
    Checking fabro-agent v0.5.0 (/home/daytona/workspace/lib/crates/fabro-agent)
    Checking fabro-ssh v0.5.0 (/home/daytona/workspace/lib/crates/fabro-ssh)
    Checking fabro-workflows v0.5.0 (/home/daytona/workspace/lib/crates/fabro-workflows)
    Checking fabro-config v0.5.0 (/home/daytona/workspace/lib/crates/fabro-config)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 06s
    ```
  - Stderr: (empty)
- **preflight_lint**: success
  - Script: `cargo clippy -- -D warnings 2>&1`
  - Stdout:
    ```
    Compiling fabro-util v0.5.0 (/home/daytona/workspace/lib/crates/fabro-util)
    Checking fabro-mcp v0.5.0 (/home/daytona/workspace/lib/crates/fabro-mcp)
    Checking fabro-tracker v0.5.0 (/home/daytona/workspace/lib/crates/fabro-tracker)
    Checking fabro-devcontainer v0.5.0 (/home/daytona/workspace/lib/crates/fabro-devcontainer)
    Checking fabro-github v0.5.0 (/home/daytona/workspace/lib/crates/fabro-github)
    Checking fabro-git-storage v0.5.0 (/home/daytona/workspace/lib/crates/fabro-git-storage)
   Compiling fabro-cli v0.5.0 (/home/daytona/workspace/lib/crates/fabro-cli)
    Checking fabro-openai-oauth v0.5.0 (/home/daytona/workspace/lib/crates/fabro-openai-oauth)
    Checking fabro-llm v0.5.0 (/home/daytona/workspace/lib/crates/fabro-llm)
    Checking fabro-agent v0.5.0 (/home/daytona/workspace/lib/crates/fabro-agent)
    Checking fabro-ssh v0.5.0 (/home/daytona/workspace/lib/crates/fabro-ssh)
    Checking fabro-workflows v0.5.0 (/home/daytona/workspace/lib/crates/fabro-workflows)
    Checking fabro-config v0.5.0 (/home/daytona/workspace/lib/crates/fabro-config)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 14.71s
    ```
  - Stderr: (empty)


Read the plan file referenced in the goal and implement every step. Make all the code changes described in the plan.