//! Wire snapshot tests pinning per-dialect encode/decode behavior.
//!
//! Each test points a real adapter at a local httpmock server, side-channels
//! the full received request (method, path, headers, body) out of an
//! `is_true` matcher closure, responds with a canned provider body, and
//! snapshots both the captured wire request (encode) and the decoded
//! canonical `Response` (decode). The codec extraction PRs must keep these
//! snapshot values identical.
//!
//! The anthropic/gemini dialects have no twin coverage, so these snapshots
//! are the only behavior net for those extractions.
//!
//! Snapshots are stored externally under `snapshots/` (via
//! `fabro_test::fabro_json_snapshot!(value)` with no inline literal) to keep
//! these source files small. Review and accept with `cargo insta`
//! (`pending-snapshots` then `accept`), per CLAUDE.md. A few tests assert two
//! snapshots in one function; insta names the second `<test>-2.snap`.

mod anthropic;
mod gemini;
mod openai_compatible;
mod openai_responses;
