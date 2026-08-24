use std::collections::HashMap;
use std::sync::Arc;

use fabro_model::{AgentProfileKind, Catalog, CodecKind, ProviderId};

pub mod anthropic;
pub mod claude5;
pub(crate) mod claude5_tools;
pub mod gemini;
pub mod gpt56;
pub mod kimi;
pub mod kimi_tools;
pub mod openai;

pub use anthropic::AnthropicProfile;
pub use claude5::Claude5Profile;
pub use gemini::GeminiProfile;
pub use gpt56::Gpt56Profile;
pub use kimi::KimiProfile;
pub use openai::OpenAiProfile;

use crate::agent_profile::AgentProfile;
use crate::apply_patch;
use crate::config::{NativeToolOptions, ToolSecrets};
use crate::native_tool::{NativeTool, ToolVocabulary};
use crate::sandbox::Sandbox;
use crate::skills::{Skill, format_skills_prompt_section};
use crate::todo_runtime::TodoRuntime;
use crate::tool_registry::ToolRegistry;
use crate::tools::{self, WebFetchSummarizer};

/// Builds a provider profile and its native tools from one configuration.
///
/// Native tool options must be supplied before [`Self::build`] because their
/// values are captured by tool executors during profile construction.
/// [`Self::build`] borrows, so one configured builder can outfit both a root
/// session and every child session it spawns with an identical tool set.
#[derive(Clone)]
pub struct AgentProfileBuilder {
    profile_kind:        AgentProfileKind,
    provider_id:         ProviderId,
    model:               String,
    catalog:             Arc<Catalog>,
    native_tool_options: NativeToolOptions,
    summarizer:          Option<WebFetchSummarizer>,
    todo_runtime:        Arc<TodoRuntime>,
}

/// Everything a profile constructor needs from the builder.
///
/// Bundled rather than passed positionally so that adding a dependency does
/// not mean editing every profile's signature -- and, more importantly, so a
/// dependency cannot reach some profiles and silently miss others. The shared
/// `todo_runtime` is exactly that case: task tools scope their list by
/// `root_session_id`, so a root and its children address one logical list and
/// must resolve it through one runtime.
pub(crate) struct ProfileDeps {
    pub options:      NativeToolOptions,
    pub summarizer:   Option<WebFetchSummarizer>,
    pub todo_runtime: Arc<TodoRuntime>,
}

impl ProfileDeps {
    /// Standalone defaults, for `Profile::new` and tests. A profile built this
    /// way owns its runtime because it has no children to share one with.
    pub(crate) fn standalone(options: NativeToolOptions) -> Self {
        Self {
            options,
            summarizer: None,
            todo_runtime: Arc::new(TodoRuntime::new()),
        }
    }
}

impl AgentProfileBuilder {
    #[must_use]
    pub fn new(
        profile_kind: AgentProfileKind,
        provider_id: ProviderId,
        model: impl Into<String>,
        catalog: Arc<Catalog>,
    ) -> Self {
        Self {
            profile_kind,
            provider_id,
            model: model.into(),
            catalog,
            native_tool_options: NativeToolOptions::for_profile(profile_kind),
            summarizer: None,
            todo_runtime: Arc::new(TodoRuntime::new()),
        }
    }

    #[must_use]
    pub fn with_tool_secrets(mut self, secrets: ToolSecrets) -> Self {
        self.native_tool_options.secrets = secrets;
        self
    }

    /// Configure the optional `web_fetch` summarizer. Profiles without
    /// `web_fetch` discard it instead of retaining an unused LLM client.
    #[must_use]
    pub fn with_web_fetch_summarizer(mut self, summarizer: Option<WebFetchSummarizer>) -> Self {
        if self.profile_kind != AgentProfileKind::Gpt56 {
            self.summarizer = summarizer;
        }
        self
    }

    #[must_use]
    pub fn build(&self) -> Box<dyn AgentProfile> {
        let model = self.model.as_str();
        let deps = ProfileDeps {
            options:      self.native_tool_options.clone(),
            summarizer:   if self.profile_kind == AgentProfileKind::Gpt56 {
                None
            } else {
                self.summarizer.clone()
            },
            todo_runtime: Arc::clone(&self.todo_runtime),
        };
        match self.profile_kind {
            AgentProfileKind::OpenAi => Box::new(
                OpenAiProfile::with_native_tools(model, &deps)
                    .with_route(self.provider_id.clone(), Arc::clone(&self.catalog)),
            ),
            AgentProfileKind::Gemini => Box::new(
                GeminiProfile::with_native_tools(model, &deps)
                    .with_provider_id(self.provider_id.clone())
                    .with_catalog(Arc::clone(&self.catalog)),
            ),
            AgentProfileKind::Anthropic => Box::new(
                AnthropicProfile::with_native_tools(model, &deps)
                    .with_provider_id(self.provider_id.clone())
                    .with_catalog(Arc::clone(&self.catalog)),
            ),
            AgentProfileKind::Claude5 => Box::new(
                Claude5Profile::with_native_tools(model, &deps)
                    .with_provider_id(self.provider_id.clone())
                    .with_catalog(Arc::clone(&self.catalog)),
            ),
            AgentProfileKind::Kimi => Box::new(
                KimiProfile::with_native_tools(model, &deps)
                    .with_provider_id(self.provider_id.clone())
                    .with_catalog(Arc::clone(&self.catalog)),
            ),
            AgentProfileKind::Gpt56 => Box::new(
                Gpt56Profile::with_native_tools(model, &deps)
                    .with_route(self.provider_id.clone(), Arc::clone(&self.catalog)),
            ),
        }
    }
}

/// Which file-editing tool a profile exposes.
///
/// `apply_patch` is a freeform grammar tool, and only the OpenAI Responses
/// codec can carry one: the `openai_compatible` codec rejects custom tool
/// definitions outright with a configuration error. A model reached through a
/// gateway such as OpenRouter therefore has to be offered the JSON-schema
/// `edit_file` instead, or every request it makes fails.
///
/// Shared by the profiles reachable over more than one codec so the rule
/// cannot drift between them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum FileEditToolKind {
    ApplyPatch,
    EditFile,
}

impl FileEditToolKind {
    pub(crate) fn for_codec(codec: CodecKind) -> Self {
        if codec == CodecKind::OpenAiResponses {
            Self::ApplyPatch
        } else {
            Self::EditFile
        }
    }

    fn native_tool(self) -> NativeTool {
        match self {
            Self::ApplyPatch => NativeTool::ApplyPatch,
            Self::EditFile => NativeTool::EditFile,
        }
    }

    fn registered_in(registry: &ToolRegistry) -> Option<Self> {
        match (
            registry
                .get_native(Self::ApplyPatch.native_tool())
                .is_some(),
            registry.get_native(Self::EditFile.native_tool()).is_some(),
        ) {
            (true, false) => Some(Self::ApplyPatch),
            (false, true) => Some(Self::EditFile),
            (false, false) | (true, true) => None,
        }
    }
}

/// Implement the [`AgentProfile`](crate::agent_profile::AgentProfile)
/// accessors that just delegate to an embedded [`BaseProfile`] named `base`.
///
/// Every profile that owns a `BaseProfile` writes the same six methods; what
/// actually distinguishes them is `build_system_prompt` and, for some,
/// `register_subagent_tools`. Types that implement the trait without a
/// `BaseProfile` -- test doubles, and the server's ask-fabro profile -- write
/// the accessors themselves, which is why this is a macro rather than a set of
/// trait defaults: there is no sensible default for a profile that has no base.
macro_rules! impl_base_profile_accessors {
    () => {
        fn profile_kind(&self) -> ::fabro_model::AgentProfileKind {
            self.base.profile_kind
        }

        fn provider_id(&self) -> ::fabro_model::ProviderId {
            self.base.provider_id.clone()
        }

        fn model(&self) -> &str {
            &self.base.model
        }

        fn catalog(&self) -> Option<&::fabro_model::Catalog> {
            self.base.catalog.as_deref()
        }

        fn tool_registry(&self) -> &$crate::tool_registry::ToolRegistry {
            &self.base.registry
        }

        fn tool_registry_mut(&mut self) -> &mut $crate::tool_registry::ToolRegistry {
            &mut self.base.registry
        }
    };
}

pub(crate) use impl_base_profile_accessors;

/// Common fields shared by all provider profiles.
///
/// Each concrete profile embeds this struct and delegates `profile_kind()`,
/// `model()`, `tool_registry()`, and `tool_registry_mut()` to it.
pub struct BaseProfile {
    pub profile_kind: AgentProfileKind,
    pub provider_id:  ProviderId,
    pub model:        String,
    pub catalog:      Option<Arc<Catalog>>,
    pub registry:     ToolRegistry,
}

impl BaseProfile {
    fn set_route(&mut self, provider_id: ProviderId, catalog: Arc<Catalog>) {
        self.provider_id = provider_id;
        self.catalog = Some(catalog);
    }

    fn provider_display_name(&self) -> String {
        self.catalog
            .as_ref()
            .and_then(|catalog| catalog.provider(&self.provider_id))
            .map_or_else(
                || self.provider_id.display_name(),
                |provider| provider.display_name.clone(),
            )
    }

    fn file_edit_tool(&self) -> Option<FileEditToolKind> {
        FileEditToolKind::registered_in(&self.registry)
    }

    /// Select the file editor supported by this route's wire codec.
    ///
    /// Returns the newly selected editor when the registry changed.
    fn configure_file_edit_tool(&mut self) -> Option<FileEditToolKind> {
        let codec = self
            .catalog
            .as_ref()?
            .effective_codec(&self.provider_id, Some(&self.model))?;
        let desired = FileEditToolKind::for_codec(codec);
        if self.file_edit_tool() == Some(desired) {
            return None;
        }

        self.registry.unregister_native(NativeTool::ApplyPatch);
        self.registry.unregister_native(NativeTool::EditFile);
        match desired {
            FileEditToolKind::ApplyPatch => {
                self.registry.register(apply_patch::make_apply_patch_tool());
            }
            FileEditToolKind::EditFile => {
                self.registry.register(tools::make_edit_file_tool());
            }
        }
        Some(desired)
    }
}

/// Additional context for building environment blocks
#[derive(Default)]
pub struct EnvContext {
    pub git_branch:         Option<String>,
    pub is_git_repo:        bool,
    pub current_date:       String,
    pub model:              String,
    pub knowledge_cutoff:   String,
    pub git_status_short:   Option<String>,
    pub git_recent_commits: Option<String>,
}

/// A checked-in MiniJinja system-prompt template and its typed inputs.
///
/// The environment block is supplied by [`assemble_system_prompt`] and cannot
/// be overridden by callers.
pub struct EmbeddedPrompt {
    name:       &'static str,
    source:     &'static str,
    inputs:     HashMap<String, toml::Value>,
    /// Vocabulary the surrounding prompt sections should name tools in.
    vocabulary: ToolVocabulary,
}

impl EmbeddedPrompt {
    #[must_use]
    pub fn new(name: &'static str, source: &'static str) -> Self {
        Self {
            name,
            source,
            inputs: HashMap::new(),
            vocabulary: ToolVocabulary::Fabro,
        }
    }

    /// Name tools in `vocabulary` in the generated sections.
    #[must_use]
    pub fn with_vocabulary(mut self, vocabulary: ToolVocabulary) -> Self {
        self.vocabulary = vocabulary;
        self
    }

    #[must_use]
    pub fn with_string(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.inputs
            .insert(name.to_string(), toml::Value::String(value.into()));
        self
    }

    #[must_use]
    pub fn with_bool(mut self, name: &'static str, value: bool) -> Self {
        self.inputs
            .insert(name.to_string(), toml::Value::Boolean(value));
        self
    }

    fn render(mut self, env_block: String) -> String {
        self.inputs
            .insert("env_block".to_string(), toml::Value::String(env_block));
        let ctx = fabro_template::TemplateContext::new().with_inputs(self.inputs);
        fabro_template::render_named(self.name, self.source, &ctx).unwrap_or_else(|err| {
            panic!(
                "embedded prompt template '{}' failed to render: {err}",
                self.name
            )
        })
    }
}

/// Assembles a complete system prompt from an embedded template and the
/// standard trailing sections.
///
/// # Panics
/// Panics if a checked-in template is invalid or references an input its
/// caller did not supply. Tests render every conditional template variant, so
/// this indicates a programmer error rather than a recoverable runtime error.
#[must_use]
pub fn assemble_system_prompt(
    template: EmbeddedPrompt,
    env: &dyn Sandbox,
    env_context: &EnvContext,
    memory: &[String],
    user_instructions: Option<&str>,
    skills: &[Skill],
) -> String {
    let env_block = build_env_context_block_with(env, env_context);
    let vocabulary = template.vocabulary;
    let prompt = template.render(env_block);

    let docs_section = if memory.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", memory.join("\n\n"))
    };
    let skills_section = {
        let s = format_skills_prompt_section(skills, vocabulary);
        if s.is_empty() {
            String::new()
        } else {
            format!("\n\n{s}")
        }
    };
    let user_section = match user_instructions {
        Some(instructions) => format!("\n\n# User Instructions\n{instructions}"),
        None => String::new(),
    };

    format!("{prompt}{docs_section}{skills_section}{user_section}")
}

#[cfg(test)]
#[must_use]
pub fn build_env_context_block(env: &dyn Sandbox) -> String {
    build_env_context_block_with(env, &EnvContext::default())
}

#[must_use]
pub fn build_env_context_block_with(env: &dyn Sandbox, ctx: &EnvContext) -> String {
    let mut lines = vec![
        "<environment>".to_string(),
        format!("Working directory: {}", env.working_directory()),
        format!("Is git repository: {}", ctx.is_git_repo),
    ];

    if let Some(ref branch) = ctx.git_branch {
        lines.push(format!("Git branch: {branch}"));
    }

    lines.push(format!("Platform: {}", env.platform()));
    lines.push(format!("OS version: {}", env.os_version()));

    if !ctx.current_date.is_empty() {
        lines.push(format!("Today's date: {}", ctx.current_date));
    }
    if !ctx.model.is_empty() {
        lines.push(format!("Model: {}", ctx.model));
    }
    if !ctx.knowledge_cutoff.is_empty() {
        lines.push(format!("Knowledge cutoff: {}", ctx.knowledge_cutoff));
    }

    if let Some(ref status) = ctx.git_status_short {
        lines.push(format!("Git status:\n{status}"));
    }
    if let Some(ref commits) = ctx.git_recent_commits {
        lines.push(format!("Recent commits:\n{commits}"));
    }

    lines.push("</environment>".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use fabro_llm::types::ToolDefinition;
    use fabro_model::catalog::LlmCatalogSettings;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::question_tools;
    use crate::subagent::{SessionFactory, SubAgentSupervisor};
    use crate::test_support::MockSandbox;
    use crate::tool_registry::ToolContext;

    fn native_tool_options(
        profile_kind: AgentProfileKind,
        has_web_search: bool,
    ) -> NativeToolOptions {
        let mut options = NativeToolOptions::for_profile(profile_kind);
        options.secrets.brave_search_api_key = has_web_search.then(|| "configured-key".to_string());
        options
    }

    fn system_prompt(profile: &dyn AgentProfile) -> String {
        let env = MockSandbox::linux();
        let context = EnvContext::default();
        profile.build_system_prompt(&env, &context, &[], None, &[])
    }

    fn register_test_subagent_tools(profile: &mut dyn AgentProfile) {
        let factory: SessionFactory = Arc::new(|| {
            panic!("should not be called while rendering a system prompt");
        });
        profile.register_subagent_tools(SubAgentSupervisor::new(3), factory, 0);
    }

    fn anthropic_profile(has_web_search: bool, has_subagents: bool) -> AnthropicProfile {
        let options = native_tool_options(AgentProfileKind::Anthropic, has_web_search);
        let deps = ProfileDeps::standalone(options);
        let mut profile = AnthropicProfile::with_native_tools("claude-haiku-4-5", &deps);
        if has_subagents {
            register_test_subagent_tools(&mut profile);
        }
        profile
    }

    fn claude5_profile(
        has_web_search: bool,
        has_subagents: bool,
        has_question: bool,
    ) -> Claude5Profile {
        let options = native_tool_options(AgentProfileKind::Claude5, has_web_search);
        let deps = ProfileDeps::standalone(options);
        let mut profile = Claude5Profile::with_native_tools("claude-sonnet-5", &deps);
        if has_subagents {
            register_test_subagent_tools(&mut profile);
        }
        if has_question {
            question_tools::register_question_tools(
                AgentProfileKind::Claude5,
                profile.tool_registry_mut(),
            );
        }
        profile
    }

    fn gemini_profile(has_web_search: bool) -> GeminiProfile {
        let options = native_tool_options(AgentProfileKind::Gemini, has_web_search);
        let deps = ProfileDeps::standalone(options);
        GeminiProfile::with_native_tools("gemini-3-flash-preview", &deps)
    }

    fn openai_apply_patch_profile(has_web_search: bool) -> OpenAiProfile {
        let options = native_tool_options(AgentProfileKind::OpenAi, has_web_search);
        let deps = ProfileDeps::standalone(options);
        OpenAiProfile::with_native_tools("gpt-5.4-mini", &deps)
    }

    fn gpt56_profile(has_web_search: bool) -> Gpt56Profile {
        let options = native_tool_options(AgentProfileKind::Gpt56, has_web_search);
        let deps = ProfileDeps::standalone(options);
        Gpt56Profile::with_native_tools("gpt-5.6-sol", &deps)
    }

    /// GPT-5.6 through an OpenAI-compatible gateway, where `apply_patch`
    /// cannot be carried and `edit_file` takes its place.
    fn gpt56_edit_file_profile(has_web_search: bool) -> Gpt56Profile {
        let options = native_tool_options(AgentProfileKind::Gpt56, has_web_search);
        let deps = ProfileDeps::standalone(options);
        let overrides: LlmCatalogSettings =
            toml::from_str("[providers.openrouter]\nenabled = true\n").unwrap();
        Gpt56Profile::with_native_tools("gpt-5.6-sol", &deps).with_route(
            ProviderId::new("openrouter"),
            Arc::new(Catalog::from_builtin_with_overrides(&overrides).unwrap()),
        )
    }

    fn openai_edit_file_profile(has_web_search: bool) -> OpenAiProfile {
        let options = native_tool_options(AgentProfileKind::OpenAi, has_web_search);
        let deps = ProfileDeps::standalone(options);
        OpenAiProfile::with_native_tools("kimi-k2.5", &deps).with_route(
            ProviderId::new("moonshot"),
            Arc::new(Catalog::from_builtin().unwrap()),
        )
    }

    /// Profiles using fabro's native tool vocabulary get the same `shell`
    /// definition, so the Bash contract does not drift between providers.
    #[test]
    fn stock_profiles_advertise_the_same_bash_shell_tool() {
        let profiles: [Box<dyn AgentProfile>; 3] = [
            Box::new(anthropic_profile(false, false)),
            Box::new(gemini_profile(false)),
            Box::new(openai_apply_patch_profile(false)),
        ];

        let definitions: Vec<ToolDefinition> = profiles
            .iter()
            .map(|profile| {
                profile
                    .tools()
                    .into_iter()
                    .find(|tool| tool.name == "shell")
                    .expect("every profile should register the shell tool")
            })
            .collect();

        for definition in &definitions {
            assert_eq!(definition.parameters, definitions[0].parameters);
            assert_eq!(definition.description, definitions[0].description);
            assert!(
                definition.description.contains("Bash"),
                "shell tool should identify Bash: {}",
                definition.description
            );
        }
    }

    /// Per-profile tool descriptions must stay per-profile. The Kimi profile
    /// rewrites several built-in descriptions; every other profile shares the
    /// registry factories, so a leak would silently reword tools for models
    /// that were never meant to see the change.
    #[test]
    fn kimi_tool_descriptions_do_not_leak_into_other_profiles() {
        use crate::agent_profile::AgentProfile;
        use crate::native_tool::NativeTool;

        let describe = |profile: &dyn AgentProfile, tool: NativeTool| {
            let vocabulary = profile.tool_registry().vocabulary();
            profile
                .tool_registry()
                .get(tool.name(vocabulary))
                .map(|t| t.definition.description.clone())
        };

        let anthropic = AnthropicProfile::new("claude-sonnet-4-6");
        let openai = OpenAiProfile::new("gpt-5.5");
        let gemini = GeminiProfile::new("gemini-3-flash-preview");
        let kimi = KimiProfile::new("kimi-k3");

        for tool in [
            NativeTool::ReadFile,
            NativeTool::WriteFile,
            NativeTool::EditFile,
            NativeTool::Shell,
            NativeTool::Grep,
            NativeTool::Glob,
        ] {
            let (Some(kimi_text), Some(anthropic_text)) =
                (describe(&kimi, tool), describe(&anthropic, tool))
            else {
                continue;
            };
            assert_ne!(
                kimi_text, anthropic_text,
                "{tool} should be reworded for Kimi only"
            );
            if tool == NativeTool::Shell {
                assert!(
                    kimi_text.to_ascii_lowercase().contains("bash"),
                    "Kimi's shell tool should still identify Bash: {kimi_text}"
                );
            }

            // The other three share the stock wording.
            for (label, other) in [
                ("openai", describe(&openai, tool)),
                ("gemini", describe(&gemini, tool)),
            ] {
                let Some(other) = other else { continue };
                assert_eq!(
                    other, anthropic_text,
                    "{label} should keep the stock {tool} description"
                );
            }

            // The Kimi-only phrasing must not appear elsewhere. Assert it is
            // present in Kimi's own description too: a one-sided check against
            // a literal silently goes vacuous the next time that wording is
            // rewritten, which is exactly how it last stopped testing anything.
            if tool == NativeTool::EditFile {
                const KIMI_EDIT_MARKER: &str = "DO NOT call Edit from memory";
                assert!(
                    kimi_text.contains(KIMI_EDIT_MARKER),
                    "Kimi's {tool} description should drill reading before an edit: {kimi_text}"
                );
                assert!(
                    !anthropic_text.contains(KIMI_EDIT_MARKER),
                    "Kimi read-before-edit drilling leaked into {tool} for other profiles"
                );
            }
        }
    }

    #[test]
    fn env_context_block_contains_platform() {
        let env = MockSandbox::linux();
        let block = build_env_context_block(&env);
        assert!(block.contains("<environment>"));
        assert!(block.contains("</environment>"));
        assert!(block.contains("linux"));
        assert!(block.contains("/home/test"));
        assert!(block.contains("Linux 6.1.0"));
    }

    #[test]
    fn env_context_block_with_extra_context() {
        let env = MockSandbox::linux();
        let ctx = EnvContext {
            git_branch:         Some("main".into()),
            is_git_repo:        true,
            current_date:       "2026-02-20".into(),
            model:              "claude-opus-4-6".into(),
            knowledge_cutoff:   "May 2025".into(),
            git_status_short:   None,
            git_recent_commits: None,
        };
        let block = build_env_context_block_with(&env, &ctx);
        assert!(block.contains("Git branch: main"));
        assert!(block.contains("Is git repository: true"));
        assert!(block.contains("Today's date: 2026-02-20"));
        assert!(block.contains("Model: claude-opus-4-6"));
        assert!(block.contains("Knowledge cutoff: May 2025"));
    }

    #[test]
    fn profile_builder_keeps_tool_availability_and_prompt_guidance_in_sync() {
        let catalog = Arc::new(Catalog::from_builtin().unwrap());
        let env = MockSandbox::linux();
        let cases = [
            (
                AgentProfileKind::OpenAi,
                ProviderId::openai(),
                "gpt-5.4-mini",
            ),
            (
                AgentProfileKind::Anthropic,
                ProviderId::anthropic(),
                "claude-haiku-4-5",
            ),
            (
                AgentProfileKind::Gemini,
                ProviderId::gemini(),
                "gemini-3-flash-preview",
            ),
            (
                AgentProfileKind::Claude5,
                ProviderId::anthropic(),
                "claude-sonnet-5",
            ),
            (AgentProfileKind::Gpt56, ProviderId::openai(), "gpt-5.6-sol"),
        ];

        for (profile_kind, provider_id, model) in cases {
            let profile = AgentProfileBuilder::new(
                profile_kind,
                provider_id.clone(),
                model,
                Arc::clone(&catalog),
            )
            .build();
            let web_search_name = NativeTool::WebSearch.name(profile.tool_registry().vocabulary());
            assert_eq!(profile.profile_kind(), profile_kind);
            assert_eq!(profile.provider_id(), provider_id);
            assert!(profile.tool_registry().get(web_search_name).is_none());
            let prompt = profile.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
            assert!(
                !prompt.contains(web_search_name),
                "{profile_kind:?} prompt advertised an unavailable tool"
            );

            let configured_builder = AgentProfileBuilder::new(
                profile_kind,
                profile.provider_id(),
                model,
                Arc::clone(&catalog),
            )
            .with_tool_secrets(ToolSecrets {
                brave_search_api_key: Some("configured-key".to_string()),
                ..ToolSecrets::default()
            });
            // Built twice: one configured builder must outfit both a root
            // session and the child sessions it spawns.
            for configured in [configured_builder.build(), configured_builder.build()] {
                assert!(configured.tool_registry().get(web_search_name).is_some());
                let prompt =
                    configured.build_system_prompt(&env, &EnvContext::default(), &[], None, &[]);
                assert!(
                    prompt.contains(web_search_name),
                    "{profile_kind:?} prompt omitted guidance for an available tool"
                );
            }
        }
    }

    /// Task tools scope their list by `root_session_id`, so a root session and
    /// every child it spawns address one logical list. `build()` runs once per
    /// session, so the runtime behind that list has to come from the builder --
    /// a per-profile runtime gives each session its own projection and its own
    /// ID counter, and the two sessions then collide on `#1` in the merged
    /// projection while neither can see the other's tasks.
    async fn assert_builder_shares_tasks_across_root_and_child(
        profile_kind: AgentProfileKind,
        model: &str,
    ) {
        let builder = AgentProfileBuilder::new(
            profile_kind,
            ProviderId::anthropic(),
            model,
            Arc::new(Catalog::from_builtin().unwrap()),
        );
        let root = builder.build();
        let child = builder.build();
        let executor = |profile: &dyn AgentProfile, name: &str| {
            Arc::clone(
                &profile
                    .tool_registry()
                    .get(name)
                    .unwrap_or_else(|| panic!("{profile_kind} should expose {name}"))
                    .executor,
            )
        };
        let root_create = executor(root.as_ref(), "TaskCreate");
        let child_create = executor(child.as_ref(), "TaskCreate");
        let child_list = executor(child.as_ref(), "TaskList");

        let env: Arc<dyn Sandbox> = Arc::new(MockSandbox::default());
        let context = |session_id: &str| ToolContext {
            write_locks:         None,
            env:                 Arc::clone(&env),
            cancel:              CancellationToken::new(),
            tool_env_provider:   None,
            session_id:          Some(session_id.to_string()),
            root_session_id:     Some("root-session".to_string()),
            tool_call_id:        None,
            agent_event_emitter: None,
        };

        root_create(
            serde_json::json!({"subject": "Parent task", "description": "Root work"}),
            context("root-session"),
        )
        .await
        .unwrap();
        child_create(
            serde_json::json!({"subject": "Child task", "description": "Child work"}),
            context("child-session"),
        )
        .await
        .unwrap();
        let tasks = child_list(serde_json::json!({}), context("child-session"))
            .await
            .unwrap();

        assert!(tasks.contains("#1 [pending] Parent task"), "{tasks}");
        assert!(tasks.contains("#2 [pending] Child task"), "{tasks}");
    }

    #[tokio::test]
    async fn claude5_builder_shares_tasks_across_root_and_child_profiles() {
        assert_builder_shares_tasks_across_root_and_child(
            AgentProfileKind::Claude5,
            "claude-sonnet-5",
        )
        .await;
    }

    #[tokio::test]
    async fn anthropic_builder_shares_tasks_across_root_and_child_profiles() {
        assert_builder_shares_tasks_across_root_and_child(
            AgentProfileKind::Anthropic,
            "claude-haiku-4-5",
        )
        .await;
    }

    #[test]
    fn profile_builder_selects_a_codec_compatible_gpt56_editor() {
        let overrides: LlmCatalogSettings =
            toml::from_str("[providers.openrouter]\nenabled = true\n").unwrap();
        let catalog = Arc::new(Catalog::from_builtin_with_overrides(&overrides).unwrap());
        let profile = AgentProfileBuilder::new(
            AgentProfileKind::Gpt56,
            ProviderId::new("openrouter"),
            "gpt-5.6-sol",
            catalog,
        )
        .build();

        assert!(profile.tool_registry().get("edit_file").is_some());
        assert!(profile.tool_registry().get("apply_patch").is_none());
    }

    #[test]
    fn anthropic_default_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&anthropic_profile(false, false)));
    }

    #[test]
    fn anthropic_web_search_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&anthropic_profile(true, false)));
    }

    #[test]
    fn anthropic_subagents_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&anthropic_profile(false, true)));
    }

    #[test]
    fn anthropic_web_search_and_subagents_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&anthropic_profile(true, true)));
    }

    #[test]
    fn claude5_default_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&claude5_profile(false, false, false)));
    }

    #[test]
    fn claude5_all_conditionals_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&claude5_profile(true, true, true)));
    }

    /// The two snapshots above pin the wording of every conditional section.
    /// This covers the six intermediate combinations, which only need to show
    /// that each section appears exactly when its tool is registered -- as
    /// snapshots they were six near-identical copies of the same prose, and any
    /// edit to the template invalidated all eight at once.
    #[test]
    fn claude5_prompt_sections_track_registered_tools() {
        for web_search in [false, true] {
            for subagents in [false, true] {
                for question in [false, true] {
                    let prompt = system_prompt(&claude5_profile(web_search, subagents, question));
                    assert_eq!(
                        prompt.contains("Use `WebSearch`"),
                        web_search,
                        "web_search={web_search} subagents={subagents} question={question}"
                    );
                    assert_eq!(
                        prompt.contains("# Background agents"),
                        subagents,
                        "web_search={web_search} subagents={subagents} question={question}"
                    );
                    assert_eq!(
                        prompt.contains("# Asking the user"),
                        question,
                        "web_search={web_search} subagents={subagents} question={question}"
                    );
                }
            }
        }
    }

    #[test]
    fn gemini_default_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&gemini_profile(false)));
    }

    #[test]
    fn gemini_web_search_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&gemini_profile(true)));
    }

    #[test]
    fn openai_apply_patch_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&openai_apply_patch_profile(false)));
    }

    #[test]
    fn openai_apply_patch_and_web_search_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&openai_apply_patch_profile(true)));
    }

    #[test]
    fn gpt56_default_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&gpt56_profile(false)));
    }

    #[test]
    fn gpt56_web_search_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&gpt56_profile(true)));
    }

    #[test]
    fn gpt56_edit_file_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&gpt56_edit_file_profile(false)));
    }

    #[test]
    fn gpt56_edit_file_and_web_search_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&gpt56_edit_file_profile(true)));
    }

    #[test]
    fn openai_edit_file_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&openai_edit_file_profile(false)));
    }

    #[test]
    fn openai_edit_file_and_web_search_prompt_snapshot() {
        insta::assert_snapshot!(system_prompt(&openai_edit_file_profile(true)));
    }
}
