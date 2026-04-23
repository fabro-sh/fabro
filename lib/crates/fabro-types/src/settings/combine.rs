use std::collections::HashMap;

use super::cli::{
    CliAuthLayer, CliAuthStrategy, CliLoggingLayer, CliTargetLayer, OutputFormat, OutputVerbosity,
};
use super::duration::Duration;
use super::features::FeaturesLayer;
use super::interp::InterpString;
use super::run::{
    AgentPermissions, ApprovalMode, DaytonaNetworkLayer, DaytonaSnapshotLayer, HookAgentMarker,
    HookEntry, HookTlsMode, InterviewProviderLayer, LocalSandboxLayer, MergeStrategy,
    ModelRefOrSplice, NotificationProviderLayer, RunArtifactsLayer, RunCheckpointLayer,
    RunGoalLayer, RunMode, RunPrepareLayer, ScmGitHubLayer, StringOrSplice, WorktreeMode,
};
use super::server::{
    GithubIntegrationStrategy, ObjectStoreLocalLayer, ObjectStoreProvider, ObjectStoreS3Layer,
    ServerApiLayer, ServerAuthGithubLayer, ServerAuthMethod, ServerListenLayer, ServerLoggingLayer,
    WebhookStrategy,
};
use super::size::Size;

pub trait Combine {
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
    u16,
    u32,
    u64,
    usize,
    i32,
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
    ObjectStoreProvider,
    ServerAuthMethod,
    WebhookStrategy,
);

impl Combine for Option<Vec<String>> {
    fn combine(self, other: Self) -> Self {
        self.or(other)
    }
}

impl Combine for Option<Vec<ServerAuthMethod>> {
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
    ServerLoggingLayer,
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

impl Combine for Vec<ModelRefOrSplice> {
    fn combine(self, other: Self) -> Self {
        splice_model_fallbacks(other, self)
    }
}

impl Combine for Vec<StringOrSplice> {
    fn combine(self, other: Self) -> Self {
        splice_events(other, self)
    }
}

impl Combine for Vec<HookEntry> {
    fn combine(self, other: Self) -> Self {
        combine_hooks(&other, self)
    }
}

fn splice_model_fallbacks(
    fallback: Vec<ModelRefOrSplice>,
    current: Vec<ModelRefOrSplice>,
) -> Vec<ModelRefOrSplice> {
    if current.is_empty() {
        return fallback;
    }
    let splice_pos = current
        .iter()
        .position(|entry| matches!(entry, ModelRefOrSplice::Splice));
    let Some(pos) = splice_pos else {
        return current;
    };
    let mut out = Vec::new();
    for (index, entry) in current.into_iter().enumerate() {
        if index == pos {
            out.extend(
                fallback
                    .iter()
                    .filter(|entry| !matches!(entry, ModelRefOrSplice::Splice))
                    .cloned(),
            );
        } else if !matches!(entry, ModelRefOrSplice::Splice) {
            out.push(entry);
        }
    }
    out
}

fn splice_events(
    fallback: Vec<StringOrSplice>,
    current: Vec<StringOrSplice>,
) -> Vec<StringOrSplice> {
    if current.is_empty() {
        return fallback;
    }
    let splice_pos = current
        .iter()
        .position(|entry| matches!(entry, StringOrSplice::Splice));
    let Some(pos) = splice_pos else {
        return current;
    };
    let mut out = Vec::new();
    for (index, entry) in current.into_iter().enumerate() {
        if index == pos {
            out.extend(
                fallback
                    .iter()
                    .filter(|entry| !matches!(entry, StringOrSplice::Splice))
                    .cloned(),
            );
        } else if !matches!(entry, StringOrSplice::Splice) {
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
        assert_option_leaf(ServerAuthMethod::Github, ServerAuthMethod::DevToken);
        assert_option_leaf(WebhookStrategy::ServerUrl, WebhookStrategy::TailscaleFunnel);
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
