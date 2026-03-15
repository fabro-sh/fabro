/// Dummy sleep inhibitor backend (no-op).
///
/// Used on platforms without a native sleep inhibitor implementation.
pub(crate) struct DummyGuard;

impl DummyGuard {
    pub(crate) fn acquire() -> Option<Self> {
        tracing::debug!("Sleep inhibitor: dummy backend (no-op)");
        Some(Self)
    }
}

impl Drop for DummyGuard {
    fn drop(&mut self) {
        tracing::debug!("Sleep inhibitor: dummy guard released");
    }
}
