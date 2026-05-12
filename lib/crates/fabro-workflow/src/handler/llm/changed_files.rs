use std::collections::HashSet;
use std::sync::Arc;

use fabro_agent::{Sandbox, shell_quote};

const DIFF_MARKER: &str = "__FABRO_CHANGED_FILES_DIFF__";
const UNTRACKED_MARKER: &str = "__FABRO_CHANGED_FILES_UNTRACKED__";

pub async fn detect_changed_files(sandbox: &Arc<dyn Sandbox>) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    let command = format!(
        "printf '%s\\n' {diff}; git diff --name-only || true; \
         printf '%s\\n' {untracked}; git ls-files --others --exclude-standard || true",
        diff = shell_quote(DIFF_MARKER),
        untracked = shell_quote(UNTRACKED_MARKER),
    );
    if let Ok(result) = sandbox
        .exec_command(&command, 30_000, None, None, None)
        .await
    {
        if result.is_success() {
            files.extend(parse_changed_files(&result.stdout));
        }
    }

    files.sort();
    files.dedup();
    files
}

pub async fn files_touched_since(
    sandbox: &Arc<dyn Sandbox>,
    files_before: &[String],
) -> (Vec<String>, Option<String>) {
    let files_after = detect_changed_files(sandbox).await;
    let files_before: HashSet<&str> = files_before.iter().map(String::as_str).collect();
    let files_touched: Vec<String> = files_after
        .into_iter()
        .filter(|file| !files_before.contains(file.as_str()))
        .collect();

    let last_file_touched = if files_touched.is_empty() {
        None
    } else {
        let quoted_files: Vec<String> =
            files_touched.iter().map(|file| shell_quote(file)).collect();
        let cmd = format!("ls -t {} | head -1", quoted_files.join(" "));
        sandbox
            .exec_command(&cmd, 5_000, None, None, None)
            .await
            .ok()
            .and_then(|result| {
                let trimmed = result.stdout.trim().to_string();
                (result.is_success() && !trimmed.is_empty()).then_some(trimmed)
            })
    };

    (files_touched, last_file_touched)
}

fn parse_changed_files(stdout: &str) -> impl Iterator<Item = String> + '_ {
    stdout.lines().filter_map(|line| {
        let trimmed = line.trim();
        (!trimmed.is_empty() && trimmed != DIFF_MARKER && trimmed != UNTRACKED_MARKER)
            .then(|| trimmed.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::parse_changed_files;

    #[test]
    fn parse_changed_files_ignores_section_markers() {
        let files = parse_changed_files(
            "__FABRO_CHANGED_FILES_DIFF__\nsrc/main.rs\n\
             __FABRO_CHANGED_FILES_UNTRACKED__\nREADME.md\n",
        )
        .collect::<Vec<_>>();

        assert_eq!(files, vec!["src/main.rs", "README.md"]);
    }
}
