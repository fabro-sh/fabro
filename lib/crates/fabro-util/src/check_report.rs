use std::fmt::Write;

use serde::Serialize;

use crate::terminal::Styles;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write `text` to `out`, rendering backtick-delimited segments with
/// `s.bold_cyan` (the conventional style for inline CLI commands).
fn write_styled_remediation(out: &mut String, text: &str, s: &Styles) {
    for (i, segment) in text.split('`').enumerate() {
        if i % 2 == 1 {
            write!(out, "{}", s.bold_cyan.apply_to(segment))
                .expect("writing to String should not fail");
        } else {
            out.push_str(segment);
        }
    }
}

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckDetail {
    pub text: String,
    pub warn: bool,
}

impl CheckDetail {
    pub fn new(text: String) -> Self {
        Self { text, warn: false }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name:        String,
    pub status:      CheckStatus,
    pub summary:     String,
    pub details:     Vec<CheckDetail>,
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckSection {
    pub title:  String,
    pub checks: Vec<CheckResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub title:    String,
    pub sections: Vec<CheckSection>,
}

impl CheckReport {
    fn all_checks(&self) -> impl Iterator<Item = &CheckResult> {
        self.sections.iter().flat_map(|s| &s.checks)
    }

    pub fn has_errors(&self) -> bool {
        self.all_checks().any(|c| c.status == CheckStatus::Error)
    }

    pub fn issue_count(&self) -> usize {
        self.all_checks()
            .filter(|c| matches!(c.status, CheckStatus::Warning | CheckStatus::Error))
            .count()
    }

    pub fn render(
        &self,
        s: &Styles,
        verbose: bool,
        footer: Option<&str>,
        max_width: Option<u16>,
    ) -> String {
        // "      • " is 8 chars of prefix before detail text
        const DETAIL_PREFIX_LEN: usize = 8;

        let mut out = String::new();
        let width = max_width.unwrap_or(80) as usize;

        let show_section_headers = self.sections.len() > 1;

        writeln!(out, "{}", s.bold.apply_to(&self.title))
            .expect("writing to String should not fail");
        writeln!(out).expect("writing to String should not fail");

        for (i, section) in self.sections.iter().enumerate() {
            if show_section_headers {
                if i > 0 {
                    writeln!(out).expect("writing to String should not fail");
                }
                writeln!(out, "  {}", s.dim.apply_to(&section.title))
                    .expect("writing to String should not fail");
            }

            for check in &section.checks {
                let (icon, color) = match check.status {
                    CheckStatus::Pass => ("[✓]", &s.green),
                    CheckStatus::Warning => ("[!]", &s.yellow),
                    CheckStatus::Error => ("[✗]", &s.red),
                };

                writeln!(
                    out,
                    "  {} {} ({})",
                    color.apply_to(icon),
                    s.bold.apply_to(&check.name),
                    check.summary,
                )
                .expect("writing to String should not fail");

                if verbose {
                    for detail in &check.details {
                        let text = if width > DETAIL_PREFIX_LEN
                            && detail.text.len() + DETAIL_PREFIX_LEN > width
                        {
                            let max_text = width - DETAIL_PREFIX_LEN - 1;
                            format!("{}…", &detail.text[..max_text])
                        } else {
                            detail.text.clone()
                        };
                        if detail.warn {
                            writeln!(out, "      • {}", s.red.apply_to(&text))
                                .expect("writing to String should not fail");
                        } else {
                            writeln!(out, "      • {text}")
                                .expect("writing to String should not fail");
                        }
                    }
                }
            }
        }

        let issues = self.issue_count();
        writeln!(out).expect("writing to String should not fail");

        if issues == 0 {
            writeln!(out, "All checks passed.").expect("writing to String should not fail");
        } else {
            writeln!(
                out,
                "Found issues in {issues} {}.",
                if issues == 1 {
                    "category"
                } else {
                    "categories"
                }
            )
            .expect("writing to String should not fail");

            let errors: Vec<_> = self
                .all_checks()
                .filter(|c| c.status == CheckStatus::Error)
                .collect();
            if !errors.is_empty() {
                writeln!(out).expect("writing to String should not fail");
                writeln!(out, "{}", s.bold.apply_to("Errors:"))
                    .expect("writing to String should not fail");
                for check in &errors {
                    write!(out, "  • {}", check.name).expect("writing to String should not fail");
                    if let Some(ref rem) = check.remediation {
                        write!(out, " — ").expect("writing to String should not fail");
                        write_styled_remediation(&mut out, rem, s);
                    }
                    writeln!(out).expect("writing to String should not fail");
                }
            }

            let warnings: Vec<_> = self
                .all_checks()
                .filter(|c| c.status == CheckStatus::Warning)
                .collect();
            if !warnings.is_empty() {
                writeln!(out).expect("writing to String should not fail");
                writeln!(out, "{}", s.bold.apply_to("Warnings:"))
                    .expect("writing to String should not fail");
                for check in &warnings {
                    write!(out, "  • {}", check.name).expect("writing to String should not fail");
                    if let Some(ref rem) = check.remediation {
                        write!(out, " — ").expect("writing to String should not fail");
                        write_styled_remediation(&mut out, rem, s);
                    }
                    writeln!(out).expect("writing to String should not fail");
                }
            }
        }

        if let Some(footer_text) = footer {
            writeln!(out).expect("writing to String should not fail");
            writeln!(out, "{footer_text}").expect("writing to String should not fail");
        }

        out
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pass_check(name: &str) -> CheckResult {
        CheckResult {
            name:        name.to_string(),
            status:      CheckStatus::Pass,
            summary:     "all good".to_string(),
            details:     vec![CheckDetail::new("everything is fine".to_string())],
            remediation: None,
        }
    }

    fn warning_check(name: &str) -> CheckResult {
        CheckResult {
            name:        name.to_string(),
            status:      CheckStatus::Warning,
            summary:     "not configured".to_string(),
            details:     vec![CheckDetail::new("missing something".to_string())],
            remediation: Some("fix it".to_string()),
        }
    }

    fn error_check(name: &str) -> CheckResult {
        CheckResult {
            name:        name.to_string(),
            status:      CheckStatus::Error,
            summary:     "broken".to_string(),
            details:     vec![CheckDetail::new("something is wrong".to_string())],
            remediation: Some("repair it".to_string()),
        }
    }

    fn report(checks: Vec<CheckResult>) -> CheckReport {
        CheckReport {
            title:    "Test Report".into(),
            sections: vec![CheckSection {
                title: String::new(),
                checks,
            }],
        }
    }

    // -- render: all-pass, no color --

    #[test]
    fn render_all_pass_no_color() {
        let r = report(vec![pass_check("Test")]);
        let out = r.render(&Styles::new(false), false, None, None);
        insta::assert_snapshot!(out, @r"
        Test Report

          [✓] Test (all good)

        All checks passed.
        ");
    }

    // -- render: warning footer --

    #[test]
    fn render_warning_footer() {
        let r = report(vec![warning_check("Optional")]);
        let out = r.render(&Styles::new(false), false, None, None);
        insta::assert_snapshot!(out, @r"
        Test Report

          [!] Optional (not configured)

        Found issues in 1 category.

        Warnings:
          • Optional — fix it
        ");
    }

    // -- render: error footer --

    #[test]
    fn render_error_footer() {
        let r = report(vec![error_check("Broken")]);
        let out = r.render(&Styles::new(false), false, None, None);
        insta::assert_snapshot!(out, @r"
        Test Report

          [✗] Broken (broken)

        Found issues in 1 category.

        Errors:
          • Broken — repair it
        ");
    }

    // -- render: verbose mode --

    #[test]
    fn render_verbose_shows_details() {
        let r = report(vec![pass_check("Verbose")]);
        let out = r.render(&Styles::new(false), true, None, None);
        insta::assert_snapshot!(out, @r"
        Test Report

          [✓] Verbose (all good)
              • everything is fine

        All checks passed.
        ");
    }

    #[test]
    fn render_default_hides_details() {
        let r = report(vec![pass_check("Verbose")]);
        let out = r.render(&Styles::new(false), false, None, None);
        assert!(!out.contains("everything is fine"));
    }

    // -- render: color --

    #[test]
    fn render_color_pass_green() {
        let r = report(vec![pass_check("Color")]);
        let out = r.render(&Styles::new(true), false, None, None);
        assert!(out.contains("\x1b[32m")); // green
    }

    #[test]
    fn render_color_warning_yellow() {
        let r = report(vec![warning_check("Color")]);
        let out = r.render(&Styles::new(true), false, None, None);
        assert!(out.contains("\x1b[33m")); // yellow
    }

    #[test]
    fn render_color_error_red() {
        let r = report(vec![error_check("Color")]);
        let out = r.render(&Styles::new(true), false, None, None);
        assert!(out.contains("\x1b[31m")); // red
    }

    // -- has_errors / issue_count --

    #[test]
    fn has_errors_false_for_warnings_only() {
        let r = report(vec![pass_check("OK"), warning_check("Warn")]);
        assert!(!r.has_errors());
    }

    #[test]
    fn has_errors_true_when_error_present() {
        let r = report(vec![pass_check("OK"), error_check("Broken")]);
        assert!(r.has_errors());
    }

    #[test]
    fn issue_count_counts_warnings_and_errors() {
        let r = report(vec![
            pass_check("OK"),
            warning_check("Warn"),
            error_check("Broken"),
        ]);
        assert_eq!(r.issue_count(), 2);
    }

    // -- render: multiple issues --

    #[test]
    fn render_multiple_issues_pluralizes() {
        let r = report(vec![warning_check("A"), error_check("B")]);
        let out = r.render(&Styles::new(false), false, None, None);
        insta::assert_snapshot!(out, @r"
        Test Report

          [!] A (not configured)
          [✗] B (broken)

        Found issues in 2 categories.

        Errors:
          • B — repair it

        Warnings:
          • A — fix it
        ");
    }

    // -- render: footer text --

    #[test]
    fn render_footer_text_when_provided() {
        let r = report(vec![pass_check("Test")]);
        let out = r.render(
            &Styles::new(false),
            false,
            Some("Run with --live to probe."),
            None,
        );
        insta::assert_snapshot!(out, @r"
        Test Report

          [✓] Test (all good)

        All checks passed.

        Run with --live to probe.
        ");
    }

    #[test]
    fn render_no_footer_when_none() {
        let r = report(vec![pass_check("Test")]);
        let out = r.render(&Styles::new(false), false, None, None);
        assert!(!out.contains("--live"));
    }

    // -- render: custom title --

    #[test]
    fn render_uses_custom_title() {
        let r = CheckReport {
            title:    "My Custom Title".into(),
            sections: vec![CheckSection {
                title:  String::new(),
                checks: vec![pass_check("Test")],
            }],
        };
        let out = r.render(&Styles::new(false), false, None, None);
        insta::assert_snapshot!(out, @r"
        My Custom Title

          [✓] Test (all good)

        All checks passed.
        ");
    }

    // -- render: truncation --

    #[test]
    fn render_truncates_long_detail_lines() {
        let r = report(vec![CheckResult {
            name:        "Test".into(),
            status:      CheckStatus::Pass,
            summary:     "ok".into(),
            details:     vec![CheckDetail::new(
                "This is a very long detail line for test".into(),
            )],
            remediation: None,
        }]);
        // max_width=40, prefix "      • " = 8 chars, so 31 chars for text + "…"
        let out = r.render(&Styles::new(false), true, None, Some(40));
        insta::assert_snapshot!(out, @r"
        Test Report

          [✓] Test (ok)
              • This is a very long detail line…

        All checks passed.
        ");
    }

    #[test]
    fn render_no_truncation_when_fits() {
        let r = report(vec![CheckResult {
            name:        "Test".into(),
            status:      CheckStatus::Pass,
            summary:     "ok".into(),
            details:     vec![CheckDetail::new("short".into())],
            remediation: None,
        }]);
        let out = r.render(&Styles::new(false), true, None, Some(80));
        assert!(out.contains("short"));
        assert!(!out.contains('…'));
    }

    // -- render: warn detail --

    #[test]
    fn render_warn_detail_uses_red() {
        let r = report(vec![CheckResult {
            name:        "Repo".into(),
            status:      CheckStatus::Pass,
            summary:     "ok".into(),
            details:     vec![CheckDetail {
                text: "Git clean: false".into(),
                warn: true,
            }],
            remediation: None,
        }]);
        let out = r.render(&Styles::new(true), true, None, None);
        assert!(out.contains("\x1b[31m"));
        assert!(out.contains("Git clean: false"));
    }

    // -- render: backtick-styled remediation --

    #[test]
    fn render_remediation_backticks_no_color() {
        let r = report(vec![CheckResult {
            name:        "Sandbox".into(),
            status:      CheckStatus::Warning,
            summary:     "not configured".into(),
            details:     Vec::new(),
            remediation: Some("Run `fabro secret set KEY` to fix".into()),
        }]);
        let out = r.render(&Styles::new(false), false, None, None);
        insta::assert_snapshot!(out, @r"
        Test Report

          [!] Sandbox (not configured)

        Found issues in 1 category.

        Warnings:
          • Sandbox — Run fabro secret set KEY to fix
        ");
    }

    #[test]
    fn render_remediation_backticks_with_color() {
        let r = report(vec![CheckResult {
            name:        "Sandbox".into(),
            status:      CheckStatus::Warning,
            summary:     "not configured".into(),
            details:     Vec::new(),
            remediation: Some("Run `fabro secret set KEY` to fix".into()),
        }]);
        let out = r.render(&Styles::new(true), false, None, None);
        // ANSI codes should wrap the command text
        assert!(out.contains("\x1b["));
        assert!(out.contains("fabro secret set KEY"));
        // backticks should not appear in the output
        assert!(!out.contains('`'));
    }
}
