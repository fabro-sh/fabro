pub fn collect_causes(error: &(dyn std::error::Error + 'static)) -> Vec<String> {
    let mut causes = Vec::new();
    let mut source = error.source();
    while let Some(cause) = source {
        causes.push(cause.to_string());
        source = cause.source();
    }
    causes
}

pub fn collect_chain(error: &(dyn std::error::Error + 'static)) -> Vec<String> {
    let mut chain = vec![error.to_string()];
    chain.extend(collect_causes(error));
    chain
}

pub fn render_with_causes(message: &str, causes: &[String]) -> String {
    if causes.is_empty() {
        return message.to_string();
    }

    let mut rendered = String::from(message);
    for cause in causes {
        rendered.push_str("\n  caused by: ");
        rendered.push_str(cause);
    }
    rendered
}
