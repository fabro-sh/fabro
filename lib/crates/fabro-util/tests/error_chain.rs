use fabro_util::error::{collect_causes, render_with_causes};

#[derive(Debug)]
struct Cause(&'static str);

impl std::fmt::Display for Cause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Cause {}

#[derive(Debug)]
struct Outer {
    message: &'static str,
    source:  Cause,
}

impl std::fmt::Display for Outer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for Outer {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[test]
fn collect_causes_walks_error_source_chain() {
    let error = Outer {
        message: "outer failure",
        source:  Cause("inner failure"),
    };

    assert_eq!(collect_causes(&error), vec!["inner failure"]);
}

#[test]
fn render_with_causes_adds_indented_caused_by_lines() {
    let rendered = render_with_causes("operation failed", &[
        "first cause".to_string(),
        "second cause".to_string(),
    ]);

    assert_eq!(
        rendered,
        "operation failed\n  caused by: first cause\n  caused by: second cause"
    );
}
