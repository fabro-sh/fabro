/// Strip markdown heading prefixes and `Plan:` prefix from a goal string.
///
/// Takes the first line, removes all leading `#` characters, then strips
/// a `Plan:` prefix if present. Returns a trimmed `&str` slice.
pub fn strip_goal_decoration(goal: &str) -> &str {
    let line = goal.lines().next().unwrap_or("");
    let line = line.trim_start_matches('#').trim();
    line.strip_prefix("Plan:").map_or(line, str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_h1() {
        assert_eq!(strip_goal_decoration("# Title"), "Title");
    }

    #[test]
    fn strips_h2() {
        assert_eq!(strip_goal_decoration("## Fix bug"), "Fix bug");
    }

    #[test]
    fn strips_h3() {
        assert_eq!(strip_goal_decoration("### Deep heading"), "Deep heading");
    }

    #[test]
    fn strips_plan_prefix() {
        assert_eq!(strip_goal_decoration("Plan: do stuff"), "do stuff");
    }

    #[test]
    fn strips_heading_and_plan_prefix() {
        assert_eq!(strip_goal_decoration("## Plan: migrate DB"), "migrate DB");
    }

    #[test]
    fn plain_text_unchanged() {
        assert_eq!(
            strip_goal_decoration("Fix the login bug"),
            "Fix the login bug"
        );
    }

    #[test]
    fn takes_first_line() {
        assert_eq!(
            strip_goal_decoration("## Plan: First\n\nMore details"),
            "First"
        );
    }

    #[test]
    fn empty_string() {
        assert_eq!(strip_goal_decoration(""), "");
    }
}
