use crate::sandbox::Sandbox;
use fabro_model::Provider;
use std::collections::HashSet;
use tracing::{debug, info, warn};

const BUDGET_BYTES: usize = 32768;

pub async fn discover_memory(
    env: &dyn Sandbox,
    git_root: &str,
    working_dir: &str,
    provider: Provider,
) -> Vec<String> {
    let directories = build_directory_walk(git_root, working_dir);

    let candidate_filenames: Vec<&str> = match provider {
        Provider::Anthropic => vec!["AGENTS.md", "CLAUDE.md"],
        Provider::OpenAi
        | Provider::Kimi
        | Provider::Zai
        | Provider::Minimax
        | Provider::Inception => {
            vec!["AGENTS.md", ".codex/instructions.md"]
        }
        Provider::Gemini => vec!["AGENTS.md", "GEMINI.md"],
    };

    let mut results = Vec::new();
    let mut budget_remaining = BUDGET_BYTES;
    let mut seen_content = HashSet::new();

    for dir in &directories {
        for filename in &candidate_filenames {
            let path = format!("{dir}/{filename}");
            if let Ok(content) = env.read_file(&path, None, None).await {
                if content.is_empty() {
                    warn!(path = %path, "Project doc file empty, skipping");
                    continue;
                }
                if !seen_content.insert(content.clone()) {
                    debug!(path = %path, "Project doc duplicate content, skipping");
                    continue;
                }
                if content.len() <= budget_remaining {
                    debug!(path = %path, size_bytes = content.len(), "Project doc loaded");
                    budget_remaining -= content.len();
                    results.push(content);
                } else if budget_remaining > 0 {
                    warn!(
                        path = %path,
                        size_bytes = content.len(),
                        budget_remaining,
                        "Project doc truncated to fit budget"
                    );
                    let truncated = truncate_to_budget(&content, budget_remaining);
                    budget_remaining = 0;
                    results.push(truncated);
                } else {
                    warn!(path = %path, size_bytes = content.len(), "Project doc skipped, budget exhausted");
                }
            }
        }
    }

    let total_bytes: usize = results.iter().map(|d| d.len()).sum();
    info!(files = results.len(), total_bytes, "Project docs loaded");

    results
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
    use super::*;
    use crate::sandbox::Sandbox;
    use crate::test_support::MockSandbox;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[tokio::test]
    async fn discovers_agents_md() {
        let mut files = HashMap::new();
        files.insert("/repo/AGENTS.md".into(), "Agent instructions".into());
        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let docs = discover_memory(env.as_ref(), "/repo", "/repo", Provider::Anthropic).await;
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0], "Agent instructions");
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
        let anthropic_docs =
            discover_memory(env.as_ref(), "/repo", "/repo", Provider::Anthropic).await;
        assert_eq!(anthropic_docs.len(), 2);
        assert_eq!(anthropic_docs[0], "agents");
        assert_eq!(anthropic_docs[1], "claude");

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files: files.clone(),
            ..Default::default()
        });
        let openai_docs = discover_memory(env.as_ref(), "/repo", "/repo", Provider::OpenAi).await;
        assert_eq!(openai_docs.len(), 2);
        assert_eq!(openai_docs[0], "agents");
        assert_eq!(openai_docs[1], "copilot");

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            files,
            ..Default::default()
        });
        let gemini_docs = discover_memory(env.as_ref(), "/repo", "/repo", Provider::Gemini).await;
        assert_eq!(gemini_docs.len(), 2);
        assert_eq!(gemini_docs[0], "agents");
        assert_eq!(gemini_docs[1], "gemini");
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
        let docs = discover_memory(env.as_ref(), "/repo", "/repo", Provider::Anthropic).await;
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0], large_content);
        // Second doc should be truncated to fit remaining budget
        assert!(docs[1].ends_with("[Project instructions truncated at 32KB]"));
        assert!(docs[0].len() + docs[1].len() <= BUDGET_BYTES);
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
        let docs = discover_memory(env.as_ref(), "/repo", "/repo", Provider::Anthropic).await;
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0], "shared instructions");
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
        let docs = discover_memory(env.as_ref(), "/repo", "/repo/src", Provider::Anthropic).await;
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0], "shared instructions");
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
        let docs =
            discover_memory(env.as_ref(), "/repo", "/repo/src/app", Provider::Anthropic).await;
        assert_eq!(docs.len(), 3);
        assert_eq!(docs[0], "root agents");
        assert_eq!(docs[1], "src agents");
        assert_eq!(docs[2], "app agents");
    }
}
