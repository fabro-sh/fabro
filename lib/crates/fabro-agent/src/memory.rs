use std::collections::HashSet;

use fabro_model::AgentProfileKind;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::error::{Error, InterruptReason};
use crate::sandbox::Sandbox;

pub const BUDGET_BYTES: usize = 32768;

/// One discovered memory file. `content` is what gets inlined into the
/// system prompt. The remaining fields describe the file for
/// observability and never carry the file's text.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryDocument {
    pub path:         String,
    pub content:      String,
    pub byte_count:   usize,
    pub loaded_bytes: usize,
    pub truncated:    bool,
}

pub async fn discover_memory(
    env: &dyn Sandbox,
    git_root: &str,
    working_dir: &str,
    profile_kind: AgentProfileKind,
    cancel_token: &CancellationToken,
) -> Result<Vec<MemoryDocument>, Error> {
    let directories = build_directory_walk(git_root, working_dir);

    let candidate_filenames: Vec<&str> = match profile_kind {
        AgentProfileKind::Anthropic => vec!["AGENTS.md", "CLAUDE.md"],
        AgentProfileKind::OpenAi => vec!["AGENTS.md", ".codex/instructions.md"],
        AgentProfileKind::Gemini => vec!["AGENTS.md", "GEMINI.md"],
    };

    let mut results: Vec<MemoryDocument> = Vec::new();
    let mut budget_remaining = BUDGET_BYTES;
    let mut seen_content = HashSet::new();

    for dir in &directories {
        for filename in &candidate_filenames {
            if cancel_token.is_cancelled() {
                return Err(Error::Interrupted(InterruptReason::Cancelled));
            }
            let path = format!("{dir}/{filename}");
            let read_result = env.read_file(&path, None, None).await;
            if cancel_token.is_cancelled() {
                return Err(Error::Interrupted(InterruptReason::Cancelled));
            }
            if let Ok(content) = read_result {
                if content.is_empty() {
                    warn!(path = %path, "Project doc file empty, skipping");
                    continue;
                }
                if !seen_content.insert(content.clone()) {
                    debug!(path = %path, "Project doc duplicate content, skipping");
                    continue;
                }
                let byte_count = content.len();
                if byte_count <= budget_remaining {
                    debug!(path = %path, size_bytes = byte_count, "Project doc loaded");
                    budget_remaining -= byte_count;
                    results.push(MemoryDocument {
                        path,
                        content,
                        byte_count,
                        loaded_bytes: byte_count,
                        truncated: false,
                    });
                } else if budget_remaining > 0 {
                    warn!(
                        path = %path,
                        size_bytes = byte_count,
                        budget_remaining,
                        "Project doc truncated to fit budget"
                    );
                    let truncated = truncate_to_budget(&content, budget_remaining);
                    let loaded_bytes = truncated.len();
                    budget_remaining = 0;
                    results.push(MemoryDocument {
                        path,
                        content: truncated,
                        byte_count,
                        loaded_bytes,
                        truncated: true,
                    });
                } else {
                    warn!(path = %path, size_bytes = byte_count, "Project doc skipped, budget exhausted");
                }
            }
        }
    }

    let total_bytes: usize = results.iter().map(|doc| doc.loaded_bytes).sum();
    info!(files = results.len(), total_bytes, "Project docs loaded");

    Ok(results)
}

fn build_directory_walk(git_root: &str, working_dir: &str) -> Vec<String> {
    let mut dirs = vec![git_root.to_string()];

    if working_dir == git_root {
        return dirs;
    }

    // Strip git_root prefix to get relative path components
    let relative = working_dir
        .strip_prefix(git_root)
        .and_then(|s| s.strip_prefix('/'))
        .unwrap_or("");

    if relative.is_empty() {
        return dirs;
    }

    let mut current = git_root.to_string();
    let parts: Vec<&str> = relative.split('/').collect();
    for part in parts {
        current = format!("{current}/{part}");
        dirs.push(current.clone());
    }

    dirs
}

fn truncate_to_budget(content: &str, budget: usize) -> String {
    const MARKER: &str = "[Project instructions truncated at 32KB]";
    if budget <= MARKER.len() {
        return MARKER[..budget].to_string();
    }
    let usable = budget - MARKER.len();
    // Find the last valid char boundary within usable bytes
    let mut end = usable;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MARKER}", &content[..end])
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::sandbox::Sandbox;
    use crate::test_support::MockSandbox;

    #[tokio::test]
    async fn discovers_agents_md() {
        let mut files = HashMap::new();
        files.insert("/repo/AGENTS.md".into(), "Agent instructions".into());
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let docs = discover_memory(
            env.as_ref(),
            "/repo",
            "/repo",
            AgentProfileKind::Anthropic,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].content, "Agent instructions");
        assert_eq!(docs[0].path, "/repo/AGENTS.md");
        assert_eq!(docs[0].byte_count, "Agent instructions".len());
        assert_eq!(docs[0].loaded_bytes, docs[0].byte_count);
        assert!(!docs[0].truncated);
    }

    #[tokio::test]
    async fn filters_by_provider() {
        let mut files = HashMap::new();
        files.insert("/repo/AGENTS.md".into(), "agents".into());
        files.insert("/repo/CLAUDE.md".into(), "claude".into());
        files.insert("/repo/.codex/instructions.md".into(), "copilot".into());
        files.insert("/repo/GEMINI.md".into(), "gemini".into());

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files: files.clone(),
            ..Default::default()
        });
        let anthropic_docs = discover_memory(
            env.as_ref(),
            "/repo",
            "/repo",
            AgentProfileKind::Anthropic,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(anthropic_docs.len(), 2);
        assert_eq!(anthropic_docs[0].content, "agents");
        assert_eq!(anthropic_docs[1].content, "claude");

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files: files.clone(),
            ..Default::default()
        });
        let openai_docs = discover_memory(
            env.as_ref(),
            "/repo",
            "/repo",
            AgentProfileKind::OpenAi,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(openai_docs.len(), 2);
        assert_eq!(openai_docs[0].content, "agents");
        assert_eq!(openai_docs[1].content, "copilot");

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let gemini_docs = discover_memory(
            env.as_ref(),
            "/repo",
            "/repo",
            AgentProfileKind::Gemini,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(gemini_docs.len(), 2);
        assert_eq!(gemini_docs[0].content, "agents");
        assert_eq!(gemini_docs[1].content, "gemini");
    }

    #[tokio::test]
    async fn truncates_at_budget() {
        let mut files = HashMap::new();
        // Create content that exceeds 32KB budget
        let large_content = "x".repeat(30000);
        let second_content = "y".repeat(5000);
        files.insert("/repo/AGENTS.md".into(), large_content.clone());
        files.insert("/repo/CLAUDE.md".into(), second_content);

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let docs = discover_memory(
            env.as_ref(),
            "/repo",
            "/repo",
            AgentProfileKind::Anthropic,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].content, large_content);
        assert!(!docs[0].truncated);
        assert_eq!(docs[0].byte_count, docs[0].content.len());
        // Second doc should be truncated to fit remaining budget
        assert!(
            docs[1]
                .content
                .ends_with("[Project instructions truncated at 32KB]")
        );
        assert!(docs[1].truncated);
        assert!(docs[1].byte_count > docs[1].content.len());
        assert!(docs[0].content.len() + docs[1].content.len() <= BUDGET_BYTES);
    }

    #[tokio::test]
    async fn deduplicates_symlinked_files() {
        let mut files = HashMap::new();
        files.insert("/repo/AGENTS.md".into(), "shared instructions".into());
        files.insert("/repo/CLAUDE.md".into(), "shared instructions".into());
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let docs = discover_memory(
            env.as_ref(),
            "/repo",
            "/repo",
            AgentProfileKind::Anthropic,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].content, "shared instructions");
    }

    #[tokio::test]
    async fn deduplicates_across_directories() {
        let mut files = HashMap::new();
        files.insert("/repo/AGENTS.md".into(), "shared instructions".into());
        files.insert("/repo/src/AGENTS.md".into(), "shared instructions".into());
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let docs = discover_memory(
            env.as_ref(),
            "/repo",
            "/repo/src",
            AgentProfileKind::Anthropic,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].content, "shared instructions");
    }

    #[tokio::test]
    async fn truncated_file_reports_byte_count_distinct_from_loaded_bytes() {
        let mut files = HashMap::new();
        // Single file larger than the budget so we hit the truncation branch
        // without any preceding consumption.
        let large_content = "x".repeat(BUDGET_BYTES + 1024);
        files.insert("/repo/AGENTS.md".into(), large_content.clone());

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let docs = discover_memory(
            env.as_ref(),
            "/repo",
            "/repo",
            AgentProfileKind::Anthropic,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(docs.len(), 1);
        assert!(docs[0].truncated);
        assert_eq!(docs[0].byte_count, large_content.len());
        assert!(docs[0].content.len() < docs[0].byte_count);
        assert!(docs[0].content.len() <= BUDGET_BYTES);
    }

    #[tokio::test]
    async fn walks_directory_hierarchy() {
        let mut files = HashMap::new();
        files.insert("/repo/AGENTS.md".into(), "root agents".into());
        files.insert("/repo/src/AGENTS.md".into(), "src agents".into());
        files.insert("/repo/src/app/AGENTS.md".into(), "app agents".into());

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let docs = discover_memory(
            env.as_ref(),
            "/repo",
            "/repo/src/app",
            AgentProfileKind::Anthropic,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(docs.len(), 3);
        assert_eq!(docs[0].content, "root agents");
        assert_eq!(docs[1].content, "src agents");
        assert_eq!(docs[2].content, "app agents");
    }
}
