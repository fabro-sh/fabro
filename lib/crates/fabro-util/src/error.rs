use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct SharedError(Arc<anyhow::Error>);

impl SharedError {
    #[must_use]
    pub fn new(err: anyhow::Error) -> Self {
        Self(Arc::new(err))
    }
}

impl std::fmt::Display for SharedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&*self.0, f)
    }
}

impl std::error::Error for SharedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

impl From<anyhow::Error> for SharedError {
    fn from(err: anyhow::Error) -> Self {
        Self::new(err)
    }
}

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

#[cfg(test)]
mod tests {
    use super::SharedError;

    #[test]
    fn shared_error_preserves_chain_without_duplicating_top_level() {
        let original = anyhow::Error::new(std::io::Error::other("leaf failure"))
            .context("middle context")
            .context("outer context");
        let original_chain = original
            .chain()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        let shared = SharedError::new(original);
        let wrapped = anyhow::Error::new(shared.clone());
        let wrapped_chain = wrapped.chain().map(ToString::to_string).collect::<Vec<_>>();

        assert_eq!(shared.to_string(), "outer context");
        assert_eq!(
            wrapped_chain
                .iter()
                .filter(|cause| cause.as_str() == "outer context")
                .count(),
            1,
            "top-level context should not be duplicated: {wrapped_chain:#?}"
        );
        assert_eq!(wrapped_chain.len(), original_chain.len());
        for original_cause in original_chain {
            assert!(
                wrapped_chain.iter().any(|cause| cause == &original_cause),
                "missing original cause {original_cause:?} in {wrapped_chain:#?}"
            );
        }
    }
}
