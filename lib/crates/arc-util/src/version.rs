use std::sync::LazyLock;

pub const ARC_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ARC_GIT_SHA: &str = env!("ARC_GIT_SHA");
pub const ARC_BUILD_DATE: &str = env!("ARC_BUILD_DATE");

pub static LONG_VERSION: LazyLock<String> =
    LazyLock::new(|| format!("{ARC_VERSION} ({ARC_GIT_SHA} {ARC_BUILD_DATE})"));
