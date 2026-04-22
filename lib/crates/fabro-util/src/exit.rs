use anyhow::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    AuthRequired,
}

// Keep the wrapper transparent so existing stderr remains unchanged while the
// exit class stays discoverable via downcast.
struct Classified {
    class: ExitClass,
    inner: Error,
}

impl Classified {
    fn class(&self) -> ExitClass {
        self.class
    }
}

impl std::fmt::Debug for Classified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.inner, f)
    }
}

impl std::fmt::Display for Classified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl std::error::Error for Classified {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

pub trait ErrorExt {
    fn classify(self, class: ExitClass) -> Error;
}

impl ErrorExt for Error {
    fn classify(self, class: ExitClass) -> Error {
        Self::new(Classified { class, inner: self })
    }
}

pub fn exit_code_for(err: &Error) -> i32 {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<Classified>())
        .map_or(1, |classified| match classified.class() {
            ExitClass::AuthRequired => 4,
        })
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::{ErrorExt, ExitClass, exit_code_for};

    #[test]
    fn unclassified_errors_default_to_exit_1() {
        assert_eq!(exit_code_for(&anyhow!("boom")), 1);
    }

    #[test]
    fn classified_errors_map_to_exit_4() {
        let err = anyhow!("boom").classify(ExitClass::AuthRequired);
        assert_eq!(exit_code_for(&err), 4);
    }

    #[test]
    fn classification_keeps_display_transparent() {
        assert_eq!(
            anyhow!("boom")
                .classify(ExitClass::AuthRequired)
                .to_string(),
            anyhow!("boom").to_string()
        );
    }

    #[test]
    fn classification_keeps_chain_length_transparent() {
        assert_eq!(
            anyhow!("boom")
                .classify(ExitClass::AuthRequired)
                .chain()
                .count(),
            anyhow!("boom").chain().count()
        );
    }

    #[test]
    fn buried_classification_still_resolves() {
        let err = anyhow!("boom")
            .context("while x")
            .classify(ExitClass::AuthRequired)
            .context("while y");
        assert_eq!(exit_code_for(&err), 4);
    }
}
