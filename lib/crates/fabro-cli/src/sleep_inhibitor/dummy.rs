use tracing::debug;

pub(crate) struct DummySleepInhibitor;

impl DummySleepInhibitor {
    pub(crate) fn acquire() -> Self {
        debug!("Sleep inhibitor: using dummy backend (no-op)");
        Self
    }
}

impl Drop for DummySleepInhibitor {
    fn drop(&mut self) {
        debug!("Sleep inhibitor: dummy backend released");
    }
}
