use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use fabro_types::settings::cli::{CliAuthStrategy, OutputFormat, OutputVerbosity};
use fabro_types::settings::run::{
    AgentPermissions, ApprovalMode, DaytonaNetworkLayer, MergeStrategy, RunMode, WorktreeMode,
};
use fabro_types::settings::server::{
    GithubIntegrationStrategy, LogDestination, ObjectStoreProvider, ServerAuthMethod,
    WebhookStrategy,
};
use fabro_types::settings::{Duration, InterpString, Size};

use super::LogFilter;
use super::cli::{CliAuthLayer, CliLoggingLayer, CliTargetLayer};
use super::features::FeaturesLayer;
use super::llm::{CostRates, CredentialRef};
use super::run::{
    DaytonaSnapshotLayer, HookAgentMarker, HookEntry, HookTlsMode, InterviewProviderLayer,
    LocalSandboxLayer, ModelRefOrSplice, NotificationProviderLayer, RunArtifactsLayer,
    RunCheckpointLayer, RunGoalLayer, RunPrepareLayer, ScmGitHubLayer, StringOrSplice,
};
use super::server::{
    ObjectStoreLocalLayer, ObjectStoreS3Layer, ServerApiLayer, ServerAuthGithubLayer,
    ServerListenLayer,
};

/// Internal merge trait used by sparse config layers inside `fabro-config`.
///
/// The `fabro_macros::Combine` derive expands against this trait via an
/// absolute path, so deriving `Combine` only works for types defined here.
pub(crate) trait Combine {
    /// Combine two values, preferring the values in `self`.
    #[must_use]
    fn combine(self, other: Self) -> Self;
}

impl<T: Combine> Combine for Option<T> {
    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Some(this), Some(fallback)) => Some(this.combine(fallback)),
            (this, fallback) => this.or(fallback),
        }
    }
}

macro_rules! impl_combine_or_option {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl Combine for Option<$ty> {
                fn combine(self, other: Self) -> Self {
                    self.or(other)
                }
            }
        )+
    };
}

impl_combine_or_option!(
    String,
    bool,
    f64,
    u16,
    u32,
    u64,
    usize,
    i32,
    i64,
    Duration,
    InterpString,
    Size,
    CliAuthStrategy,
    OutputFormat,
    OutputVerbosity,
    AgentPermissions,
    ApprovalMode,
    HookAgentMarker,
    HookTlsMode,
    MergeStrategy,
    RunMode,
    WorktreeMode,
    GithubIntegrationStrategy,
    LogDestination,
    NaiveDate,
    ObjectStoreProvider,
    ServerAuthMethod,
    WebhookStrategy,
    LogFilter,
);

impl Combine for Option<Vec<String>> {
    fn combine(self, other: Self) -> Self {
        self.or(other)
    }
}

impl Combine for Option<Vec<CredentialRef>> {
    fn combine(self, other: Self) -> Self {
        self.or(other)
    }
}

impl Combine for Option<Vec<ServerAuthMethod>> {
    fn combine(self, other: Self) -> Self {
        self.or(other)
    }
}

impl Combine for Option<BTreeMap<String, CostRates>> {
    fn combine(self, other: Self) -> Self {
        self.or(other)
    }
}

impl Combine for Option<HashMap<String, toml::Value>> {
    fn combine(self, other: Self) -> Self {
        self.or(other)
    }
}

macro_rules! impl_combine_self {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl Combine for $ty {
                fn combine(self, _other: Self) -> Self {
                    self
                }
            }
        )+
    };
}

impl_combine_self!(
    CliAuthLayer,
    CliLoggingLayer,
    CliTargetLayer,
    FeaturesLayer,
    DaytonaNetworkLayer,
    DaytonaSnapshotLayer,
    InterviewProviderLayer,
    LocalSandboxLayer,
    NotificationProviderLayer,
    RunArtifactsLayer,
    RunGoalLayer,
    RunPrepareLayer,
    ScmGitHubLayer,
    ObjectStoreLocalLayer,
    ObjectStoreS3Layer,
    ServerApiLayer,
    ServerAuthGithubLayer,
    ServerListenLayer,
);

impl Combine for RunCheckpointLayer {
    fn combine(self, other: Self) -> Self {
        if self.exclude_globs.is_empty() {
            other
        } else {
            self
        }
    }
}

/// An element of a splice-aware sequence: either a regular value or the
/// `...` marker that asks the combiner to expand the fallback list inline.
trait SpliceMarker {
    fn is_splice(&self) -> bool;
}

impl SpliceMarker for ModelRefOrSplice {
    fn is_splice(&self) -> bool {
        matches!(self, Self::Splice)
    }
}

impl SpliceMarker for StringOrSplice {
    fn is_splice(&self) -> bool {
        matches!(self, Self::Splice)
    }
}

impl<T: SpliceMarker + Clone> Combine for Vec<T> {
    fn combine(self, other: Self) -> Self {
        splice_combine(other, self)
    }
}

impl Combine for Vec<HookEntry> {
    fn combine(self, other: Self) -> Self {
        combine_hooks(&other, self)
    }
}

fn splice_combine<T: SpliceMarker + Clone>(fallback: Vec<T>, current: Vec<T>) -> Vec<T> {
    if current.is_empty() {
        return fallback;
    }
    let Some(pos) = current.iter().position(T::is_splice) else {
        return current;
    };
    let mut out = Vec::with_capacity(current.len() - 1 + fallback.len());
    for (index, entry) in current.into_iter().enumerate() {
        if index == pos {
            out.extend(fallback.iter().filter(|entry| !entry.is_splice()).cloned());
        } else if !entry.is_splice() {
            out.push(entry);
        }
    }
    out
}

fn combine_hooks(fallback: &[HookEntry], current: Vec<HookEntry>) -> Vec<HookEntry> {
    let mut out = Vec::with_capacity(fallback.len() + current.len());
    let mut appended_ids = Vec::new();

    for fallback_entry in fallback {
        if let Some(id) = &fallback_entry.id {
            if let Some(replacement) = current
                .iter()
                .find(|entry| entry.id.as_deref() == Some(id.as_str()))
            {
                out.push(replacement.clone());
                appended_ids.push(id.clone());
                continue;
            }
        }
        out.push(fallback_entry.clone());
    }

    for current_entry in current {
        if let Some(id) = &current_entry.id {
            if appended_ids.contains(id) {
                continue;
            }
        }
        out.push(current_entry);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, fabro_macros::Combine)]
    struct FieldMergeLayer {
        a: Option<u32>,
        b: Option<u32>,
    }

    #[derive(Debug, PartialEq)]
    struct WholeReplaceLayer {
        a: Option<u32>,
        b: Option<u32>,
    }

    impl Combine for WholeReplaceLayer {
        fn combine(self, _other: Self) -> Self {
            self
        }
    }

    #[track_caller]
    fn assert_option_leaf<T>(this: T, fallback: T)
    where
        T: Clone + std::fmt::Debug + PartialEq,
        Option<T>: Combine,
    {
        assert_eq!(
            Some(this.clone()).combine(Some(fallback.clone())),
            Some(this)
        );
        assert_eq!(
            Option::<T>::None.combine(Some(fallback.clone())),
            Some(fallback)
        );
    }

    #[test]
    fn option_leaf_types_prefer_self_or_fallback() {
        assert_option_leaf("this".to_string(), "fallback".to_string());
        assert_option_leaf(true, false);
        assert_option_leaf(1_u16, 2_u16);
        assert_option_leaf(1_u32, 2_u32);
        assert_option_leaf(1_u64, 2_u64);
        assert_option_leaf(1_usize, 2_usize);
        assert_option_leaf(1_i32, 2_i32);
        assert_option_leaf(Duration::from_secs(1), Duration::from_secs(2));
        assert_option_leaf(InterpString::parse("this"), InterpString::parse("fallback"));
        assert_option_leaf(Size::from_bytes(1), Size::from_bytes(2));
        assert_option_leaf(CliAuthStrategy::None, CliAuthStrategy::Jwt);
        assert_option_leaf(OutputFormat::Json, OutputFormat::Text);
        assert_option_leaf(OutputVerbosity::Quiet, OutputVerbosity::Verbose);
        assert_option_leaf(AgentPermissions::ReadOnly, AgentPermissions::Full);
        assert_option_leaf(ApprovalMode::Auto, ApprovalMode::Prompt);
        assert_option_leaf(HookAgentMarker::Enabled, HookAgentMarker::Enabled);
        assert_option_leaf(HookTlsMode::NoVerify, HookTlsMode::Verify);
        assert_option_leaf(MergeStrategy::Rebase, MergeStrategy::Squash);
        assert_option_leaf(RunMode::DryRun, RunMode::Normal);
        assert_option_leaf(WorktreeMode::Always, WorktreeMode::Never);
        assert_option_leaf(
            GithubIntegrationStrategy::App,
            GithubIntegrationStrategy::Token,
        );
        assert_option_leaf(ObjectStoreProvider::S3, ObjectStoreProvider::Local);
        assert_option_leaf(LogDestination::Stdout, LogDestination::File);
        assert_option_leaf(ServerAuthMethod::Github, ServerAuthMethod::DevToken);
        assert_option_leaf(WebhookStrategy::ServerUrl, WebhookStrategy::TailscaleFunnel);
        assert_option_leaf(
            LogFilter::parse("debug").unwrap(),
            LogFilter::parse("info").unwrap(),
        );
        assert_option_leaf(vec!["this".to_string()], vec!["fallback".to_string()]);
        assert_option_leaf(vec![ServerAuthMethod::Github], vec![
            ServerAuthMethod::DevToken,
        ]);
        assert_option_leaf(
            HashMap::from([("this".to_string(), toml::Value::String("value".to_string()))]),
            HashMap::from([(
                "fallback".to_string(),
                toml::Value::String("value".to_string()),
            )]),
        );
    }

    #[test]
    fn recursive_option_combines_inner_fields() {
        let this = Some(FieldMergeLayer {
            a: Some(1),
            b: None,
        });
        let fallback = Some(FieldMergeLayer {
            a: Some(2),
            b: Some(3),
        });

        assert_eq!(
            this.combine(fallback),
            Some(FieldMergeLayer {
                a: Some(1),
                b: Some(3),
            })
        );
    }

    #[test]
    fn whole_replace_inner_does_not_inherit_fallback_fields() {
        let this = Some(WholeReplaceLayer {
            a: Some(1),
            b: None,
        });
        let fallback = Some(WholeReplaceLayer {
            a: Some(2),
            b: Some(3),
        });

        assert_eq!(
            this.combine(fallback),
            Some(WholeReplaceLayer {
                a: Some(1),
                b: None,
            })
        );
    }
}
