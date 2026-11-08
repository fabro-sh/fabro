use std::fmt::Write as _;

use crate::client::JiraIssue;

#[must_use]
pub fn issue_url(site_url: &str, key: &str) -> String {
    format!("{site_url}/browse/{key}")
}

#[must_use]
pub fn run_title(issue: &JiraIssue) -> String {
    format!("{} {}", issue.key, issue.summary)
}

#[must_use]
pub fn render_goal(site_url: &str, issue: &JiraIssue) -> String {
    let mut out = format!(
        "# {}\n{}\n\nStatus: {}\n",
        run_title(issue),
        issue_url(site_url, &issue.key),
        issue.status
    );
    if let Some(issue_type) = &issue.issue_type {
        let _ = writeln!(out, "Type: {issue_type}");
    }
    if let Some(priority) = &issue.priority {
        let _ = writeln!(out, "Priority: {priority}");
    }
    if !issue.labels.is_empty() {
        let _ = writeln!(out, "Labels: {}", issue.labels.join(", "));
    }
    if let Some(description) = issue.description.as_deref().filter(|d| !d.is_empty()) {
        let _ = write!(out, "\n## Description\n\n{description}\n");
    }
    if !issue.comments.is_empty() {
        out.push_str("\n## Comments\n\n");
        for comment in &issue.comments {
            let _ = write!(
                out,
                "**{}** ({}):\n{}\n\n",
                comment.author, comment.created, comment.body
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::JiraComment;

    fn issue() -> JiraIssue {
        JiraIssue {
            key:         "ABC-1".into(),
            summary:     "Fix login".into(),
            description: Some("Login fails on Safari.".into()),
            status:      "To Do".into(),
            issue_type:  Some("Bug".into()),
            priority:    Some("High".into()),
            labels:      vec!["auth".into(), "backend".into()],
            comments:    vec![JiraComment {
                author:  "alice".into(),
                created: "2026-11-01T10:00:00.000+0000".into(),
                body:    "Reproduced.".into(),
            }],
        }
    }

    #[test]
    fn render_goal_includes_key_url_summary_fields_description_and_comments() {
        let goal = render_goal("https://acme.atlassian.net", &issue());
        assert!(goal.contains("# ABC-1 Fix login"));
        assert!(goal.contains("https://acme.atlassian.net/browse/ABC-1"));
        assert!(goal.contains("Type: Bug"));
        assert!(goal.contains("Priority: High"));
        assert!(goal.contains("Labels: auth, backend"));
        assert!(goal.contains("Login fails on Safari."));
        assert!(goal.contains("## Comments"));
        assert!(goal.contains("**alice** (2026-11-01T10:00:00.000+0000):\nReproduced."));
    }

    #[test]
    fn render_goal_without_description_or_comments_still_has_header() {
        let mut issue = issue();
        issue.description = None;
        issue.comments.clear();
        let goal = render_goal("https://acme.atlassian.net", &issue);
        assert!(goal.contains("# ABC-1 Fix login"));
        assert!(goal.contains("https://acme.atlassian.net/browse/ABC-1"));
        assert!(!goal.contains("## Comments"));
        assert!(!goal.contains("## Description"));
    }

    #[test]
    fn run_title_is_key_then_summary() {
        assert_eq!(run_title(&issue()), "ABC-1 Fix login");
    }
}
