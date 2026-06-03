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

mod anthropic;
mod gemini;
mod openai_compatible;
mod openai_responses;
