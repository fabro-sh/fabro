use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fabro_graphviz::graph::{AttrValue, Graph};
use fabro_template::{TemplateContext, TemplateSource, TemplateStore};
use fabro_types::ManifestPath;
use fabro_validate::Diagnostic;

use super::Transform;
use crate::error::Error;
use crate::file_resolver::{FileResolver, FileResolverTemplateStore, ResolvedFile};
use crate::static_reference::{ReferenceKind, validate_static_reference};
use crate::transforms::variable_expansion::{
    RenderMode, TemplateRenderStore, TemplateRenderTarget, TemplateTransform,
    render_template_for_target,
};

/// Resolve a potential `@path` file reference.
///
/// If `value` starts with `@` and the referenced file exists locally, the file
/// contents are returned (inlined). Otherwise the original value is returned
/// unchanged.
pub fn resolve_file_ref(
    value: &str,
    current_dir: &Path,
    resolver: &dyn FileResolver,
) -> Result<String, Error> {
    let Some(path_str) = value.strip_prefix('@') else {
        return Ok(value.to_string());
    };
    validate_static_reference(path_str, ReferenceKind::FileInline)
        .map_err(|error| Error::Validation(error.to_string()))?;
    Ok(resolver
        .resolve(current_dir, path_str)
        .map_or_else(|| value.to_string(), |resolved| resolved.content))
}

fn parent_dir_or_dot(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

pub(crate) fn template_render_store(
    current_dir: &Path,
    resolver: Arc<dyn FileResolver>,
    source_name: Option<&str>,
    content: &str,
) -> Result<TemplateRenderStore, Error> {
    let root = template_root_for_current_dir(current_dir)?;
    let source_path = template_source_path_for_current_dir(current_dir, source_name, &root)?;
    let base_dir = template_store_base_dir(current_dir);
    Ok(TemplateRenderStore::new(
        TemplateSource::new(source_path, root, content.to_owned()),
        Arc::new(FileResolverTemplateStore::new(base_dir, resolver)),
    ))
}

fn template_store_base_dir(current_dir: &Path) -> PathBuf {
    if current_dir.is_absolute() {
        current_dir.to_path_buf()
    } else {
        PathBuf::from(".")
    }
}

fn template_root_for_current_dir(current_dir: &Path) -> Result<ManifestPath, Error> {
    if current_dir.is_absolute() {
        return manifest_path(".");
    }
    manifest_path_from_path(current_dir)
}

fn template_source_path_for_current_dir(
    current_dir: &Path,
    source_name: Option<&str>,
    root: &ManifestPath,
) -> Result<ManifestPath, Error> {
    if let Some(source_name) = source_name {
        let source_path = Path::new(source_name);
        if source_path.is_absolute() {
            if let Some(path) = ManifestPath::from_absolute(source_path, current_dir) {
                return Ok(path);
            }
        } else if let Some(path) = ManifestPath::from_wire(source_name) {
            if root.as_path().as_os_str().is_empty() || path.starts_with(root) {
                return Ok(path);
            }
            if let Some(path) = ManifestPath::from_reference(root.as_path(), source_name) {
                return Ok(path);
            }
        }
    }
    ManifestPath::from_reference(root.as_path(), "workflow.fabro")
        .ok_or_else(|| Error::Validation("invalid workflow template source path".to_string()))
}

fn manifest_path(value: &str) -> Result<ManifestPath, Error> {
    ManifestPath::from_wire(value)
        .ok_or_else(|| Error::Validation(format!("invalid manifest path: {value}")))
}

fn manifest_path_from_path(path: &Path) -> Result<ManifestPath, Error> {
    let value = path
        .to_str()
        .ok_or_else(|| Error::Validation(format!("invalid UTF-8 path: {}", path.display())))?;
    manifest_path(value)
}

fn manifest_parent_or_dot(path: &ManifestPath) -> Result<ManifestPath, Error> {
    manifest_path_from_path(path.parent_or_dot())
}

fn manifest_path_is_within_root(path: &ManifestPath, root: &ManifestPath) -> bool {
    if root.as_path().as_os_str().is_empty() {
        return !path
            .as_path()
            .components()
            .next()
            .is_some_and(|component| matches!(component, std::path::Component::ParentDir));
    }
    path.starts_with(root)
}

/// Inlines `@file` references in node prompts and the graph-level goal.
pub struct FileInliningTransform {
    current_dir:   PathBuf,
    resolver:      Arc<dyn FileResolver>,
    inputs:        HashMap<String, toml::Value>,
    source_name:   Option<String>,
    source_text:   Option<String>,
    goal_override: Option<String>,
    render_mode:   RenderMode,
}

impl FileInliningTransform {
    #[must_use]
    pub fn new(current_dir: PathBuf, resolver: Arc<dyn FileResolver>) -> Self {
        Self {
            current_dir,
            resolver,
            inputs: HashMap::new(),
            source_name: None,
            source_text: None,
            goal_override: None,
            render_mode: RenderMode::Strict,
        }
    }

    #[must_use]
    pub fn with_template_options(
        mut self,
        inputs: HashMap<String, toml::Value>,
        source_name: Option<String>,
        source_text: Option<String>,
        render_mode: RenderMode,
    ) -> Self {
        self.inputs = inputs;
        self.source_name = source_name;
        self.source_text = source_text;
        self.render_mode = render_mode;
        self
    }

    #[must_use]
    pub fn with_goal_override(mut self, goal: Option<String>) -> Self {
        self.goal_override = goal;
        self
    }

    pub(crate) fn apply_with_diagnostics(
        &self,
        graph: Graph,
    ) -> Result<(Graph, Vec<Diagnostic>), Error> {
        let mut graph = graph;
        let mut diagnostics = Vec::new();
        self.inline_graph_goal(&mut graph, &mut diagnostics)?;

        let resolved_goal = match &self.goal_override {
            Some(goal) => goal.clone(),
            None => TemplateTransform {
                inputs:      self.inputs.clone(),
                source_name: self.source_name.clone(),
                source_text: self.source_text.clone(),
                render_mode: self.render_mode,
            }
            .resolved_goal(&graph, &mut diagnostics)?,
        };
        let ctx = TemplateContext::new()
            .with_goal(resolved_goal)
            .with_inputs(self.inputs.clone());

        for (node_id, node) in &mut graph.nodes {
            let Some(AttrValue::String(prompt)) = node.attrs.get("prompt") else {
                continue;
            };
            let target = TemplateRenderTarget::node_attr(
                self.source_name.clone(),
                node_id.clone(),
                "prompt",
            )
            .with_source_origin(self.source_text.as_deref(), prompt)
            .with_template_store(template_render_store(
                &self.current_dir,
                Arc::clone(&self.resolver),
                self.source_name.as_deref(),
                prompt,
            )?);
            let rendered = render_template_for_target(
                prompt,
                &ctx,
                self.render_mode,
                &target,
                &mut diagnostics,
            )?;
            let value = self
                .render_resolved_file_ref(&rendered, &ctx, target, &mut diagnostics)?
                .unwrap_or(rendered);
            node.attrs
                .insert("prompt".to_string(), AttrValue::String(value));
        }

        Ok((graph, diagnostics))
    }

    fn inline_graph_goal(
        &self,
        graph: &mut Graph,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<(), Error> {
        let Some(AttrValue::String(goal)) = graph.attrs.get("goal") else {
            return Ok(());
        };
        let ctx = TemplateContext::for_input_scan(self.inputs.clone());
        let target = TemplateRenderTarget::graph_attr(self.source_name.clone(), "goal")
            .with_source_origin(self.source_text.as_deref(), goal)
            .with_template_store(template_render_store(
                &self.current_dir,
                Arc::clone(&self.resolver),
                self.source_name.as_deref(),
                goal,
            )?);
        let rendered =
            render_template_for_target(goal, &ctx, self.render_mode, &target, diagnostics)?;
        let value = self
            .render_resolved_file_ref(&rendered, &ctx, target, diagnostics)?
            .unwrap_or(rendered);
        graph
            .attrs
            .insert("goal".to_string(), AttrValue::String(value));
        Ok(())
    }

    fn render_resolved_file_ref(
        &self,
        value: &str,
        ctx: &TemplateContext,
        owner_target: TemplateRenderTarget,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<Option<String>, Error> {
        let Some(path_str) = value.strip_prefix('@') else {
            return Ok(None);
        };
        validate_static_reference(path_str, ReferenceKind::FileInline)
            .map_err(|error| Error::Validation(error.to_string()))?;
        let Some(resolved) = self.resolver.resolve(&self.current_dir, path_str) else {
            return Ok(None);
        };
        let (source, store) = self.template_source_for_resolved_file(&resolved)?;
        let target = owner_target
            .with_source_name(resolved.path.display().to_string())
            .with_source_origin(Some(&resolved.content), &resolved.content)
            .with_template_store(TemplateRenderStore::new(source, store));
        Ok(Some(render_file_contents(
            &resolved,
            ctx,
            self.render_mode,
            &target,
            diagnostics,
        )?))
    }

    fn template_root_for_resolved_file(&self, path: &Path) -> PathBuf {
        let parent = parent_dir_or_dot(path);
        if self.current_dir.is_absolute() && path.starts_with(&self.current_dir) {
            self.current_dir.clone()
        } else {
            parent
        }
    }

    fn template_source_for_resolved_file(
        &self,
        resolved: &ResolvedFile,
    ) -> Result<(TemplateSource, Arc<dyn TemplateStore>), Error> {
        if resolved.path.is_absolute() {
            let root_dir = self.template_root_for_resolved_file(&resolved.path);
            let path = ManifestPath::from_absolute(&resolved.path, &root_dir).ok_or_else(|| {
                Error::Validation(format!(
                    "invalid resolved template path: {}",
                    resolved.path.display()
                ))
            })?;
            return Ok((
                TemplateSource::new(path, manifest_path(".")?, resolved.content.clone()),
                Arc::new(FileResolverTemplateStore::new(
                    root_dir,
                    Arc::clone(&self.resolver),
                )),
            ));
        }

        let path = manifest_path_from_path(&resolved.path)?;
        let current_root = template_root_for_current_dir(&self.current_dir)?;
        let root = if manifest_path_is_within_root(&path, &current_root) {
            current_root
        } else {
            manifest_parent_or_dot(&path)?
        };
        Ok((
            TemplateSource::new(path, root, resolved.content.clone()),
            Arc::new(FileResolverTemplateStore::new(
                PathBuf::from("."),
                Arc::clone(&self.resolver),
            )),
        ))
    }
}

impl Transform for FileInliningTransform {
    fn apply(&self, graph: Graph) -> Result<Graph, Error> {
        let (graph, diagnostics) = self.apply_with_diagnostics(graph)?;
        if !diagnostics.is_empty() {
            return Err(Error::ValidationFailed { diagnostics });
        }
        Ok(graph)
    }
}

pub(crate) fn render_file_contents(
    resolved: &ResolvedFile,
    ctx: &TemplateContext,
    render_mode: RenderMode,
    target: &TemplateRenderTarget,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<String, Error> {
    render_template_for_target(&resolved.content, ctx, render_mode, target, diagnostics)
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::disallowed_methods,
        reason = "These unit tests use the real git CLI to build repositories for file-inlining transform coverage."
    )]

    use std::sync::Arc;

    use fabro_graphviz::graph::{AttrValue, Graph, Node};
    use fabro_template::{TemplateRenderMode, TemplateSource, render_source};
    use fabro_types::ManifestPath;

    use super::*;
    use crate::file_resolver::{BundleFileResolver, FilesystemFileResolver};

    fn manifest_path(value: &str) -> ManifestPath {
        ManifestPath::from_wire(value).expect("path should parse")
    }

    #[test]
    fn resolve_file_ref_passthrough_non_at() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_file_ref(
                "hello world",
                dir.path(),
                &FilesystemFileResolver::new(None),
            )
            .unwrap(),
            "hello world"
        );
    }

    #[test]
    fn resolve_file_ref_passthrough_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_file_ref(
                "@nonexistent.md",
                dir.path(),
                &FilesystemFileResolver::new(None),
            )
            .unwrap(),
            "@nonexistent.md"
        );
    }

    #[test]
    fn resolve_file_ref_inlines_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("prompt.md"), "inlined content").unwrap();

        assert_eq!(
            resolve_file_ref("@prompt.md", dir.path(), &FilesystemFileResolver::new(None)).unwrap(),
            "inlined content"
        );
    }

    #[test]
    fn file_inlining_transform_inlines_prompt_and_goal() {
        let dir = tempfile::tempdir().unwrap();
        // Init repo
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        std::fs::write(dir.path().join("prompt.md"), "Do the work").unwrap();
        std::fs::write(dir.path().join("goal.md"), "Ship feature").unwrap();

        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("@goal.md".to_string()),
        );
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("@prompt.md".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let transform = FileInliningTransform::new(
            dir.path().to_path_buf(),
            Arc::new(FilesystemFileResolver::new(None)),
        );
        let graph = transform.apply(graph).unwrap();

        assert_eq!(
            graph.nodes["work"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("Do the work")
        );
        assert_eq!(
            graph.attrs.get("goal").and_then(AttrValue::as_str),
            Some("Ship feature")
        );
    }

    #[test]
    fn file_inlining_transform_resolves_minijinja_includes_for_prompts_and_goal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("prompts")).unwrap();
        std::fs::create_dir_all(dir.path().join("goals")).unwrap();
        std::fs::write(
            dir.path().join("prompts/work.md"),
            r#"{% include "work.tpl.md" %}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("prompts/work.tpl.md"), "file prompt").unwrap();
        std::fs::write(dir.path().join("inline.tpl.md"), "inline prompt").unwrap();
        std::fs::write(
            dir.path().join("goals/goal.md"),
            r#"{% include "goal.tpl.md" %}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("goals/goal.tpl.md"), "included goal").unwrap();

        let mut graph = Graph::new("test");
        graph.attrs.insert(
            "goal".to_string(),
            AttrValue::String("@goals/goal.md".to_string()),
        );
        let mut file_prompt = Node::new("file_prompt");
        file_prompt.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("@prompts/work.md".to_string()),
        );
        graph.nodes.insert("file_prompt".to_string(), file_prompt);
        let mut inline_prompt = Node::new("inline_prompt");
        inline_prompt.attrs.insert(
            "prompt".to_string(),
            AttrValue::String(r#"{% include "inline.tpl.md" %}"#.to_string()),
        );
        graph
            .nodes
            .insert("inline_prompt".to_string(), inline_prompt);

        let transform = FileInliningTransform::new(
            dir.path().to_path_buf(),
            Arc::new(FilesystemFileResolver::new(None)),
        );
        let graph = transform.apply(graph).unwrap();

        assert_eq!(
            graph.nodes["file_prompt"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("file prompt")
        );
        assert_eq!(
            graph.nodes["inline_prompt"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("inline prompt")
        );
        assert_eq!(
            graph.attrs.get("goal").and_then(AttrValue::as_str),
            Some("included goal")
        );
    }

    #[test]
    fn file_resolver_template_store_renders_sibling_partial_under_root() {
        let resolver = Arc::new(BundleFileResolver::new(HashMap::from([(
            manifest_path("prompts/partials/audit.partial.tpl"),
            "shared partial".to_string(),
        )])));
        let store = FileResolverTemplateStore::new(PathBuf::from("."), resolver);
        let source = TemplateSource::new(
            manifest_path("prompts/audits/audit.prompt.md"),
            manifest_path("prompts"),
            r#"{% include "../partials/audit.partial.tpl" %}"#,
        );

        let rendered = render_source(
            &source,
            &TemplateContext::new(),
            Arc::new(store),
            TemplateRenderMode::Strict,
        )
        .unwrap();

        assert_eq!(rendered, "shared partial");
    }

    #[test]
    fn file_resolver_template_store_rejects_escaping_include() {
        let resolver = Arc::new(BundleFileResolver::new(HashMap::from([(
            manifest_path("outside.md"),
            "outside".to_string(),
        )])));
        let store = FileResolverTemplateStore::new(PathBuf::from("."), resolver);
        let source = TemplateSource::new(
            manifest_path("prompts/audits/audit.prompt.md"),
            manifest_path("prompts"),
            r#"{% include "../../outside.md" %}"#,
        );

        let err = render_source(
            &source,
            &TemplateContext::new(),
            Arc::new(store),
            TemplateRenderMode::Strict,
        )
        .unwrap_err();

        assert!(matches!(err, fabro_template::TemplateError::Load { .. }));
    }

    #[test]
    fn resolve_file_ref_expands_tilde() {
        let home = dirs::home_dir().expect("home dir must exist");
        let test_file = home.join(".fabro_test_tilde_tmp");
        std::fs::write(&test_file, "tilde content").unwrap();
        let _cleanup = scopeguard::guard((), |()| {
            let _ = std::fs::remove_file(&test_file);
        });

        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            resolve_file_ref(
                "@~/.fabro_test_tilde_tmp",
                dir.path(),
                &FilesystemFileResolver::new(None),
            )
            .unwrap(),
            "tilde content"
        );
    }

    #[test]
    fn resolve_file_ref_resolves_dotdot() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.md"), "dotdot content").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        assert_eq!(
            resolve_file_ref(
                "@subdir/../file.md",
                dir.path(),
                &FilesystemFileResolver::new(None),
            )
            .unwrap(),
            "dotdot content"
        );
    }

    #[test]
    fn resolve_file_ref_falls_back_to_fallback_dir() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(fallback.path().join("shared.md"), "shared content").unwrap();

        assert_eq!(
            resolve_file_ref(
                "@shared.md",
                base.path(),
                &FilesystemFileResolver::new(Some(fallback.path().to_path_buf())),
            )
            .unwrap(),
            "shared content"
        );
    }

    #[test]
    fn resolve_file_ref_base_dir_takes_precedence_over_fallback() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(base.path().join("prompt.md"), "base content").unwrap();
        std::fs::write(fallback.path().join("prompt.md"), "fallback content").unwrap();

        assert_eq!(
            resolve_file_ref(
                "@prompt.md",
                base.path(),
                &FilesystemFileResolver::new(Some(fallback.path().to_path_buf())),
            )
            .unwrap(),
            "base content"
        );
    }

    #[test]
    fn resolve_file_ref_no_fallback_for_tilde_path() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(fallback.path().join("file.md"), "fallback").unwrap();

        // Tilde path to nonexistent file should return original value, not try fallback
        let result = resolve_file_ref(
            "@~/nonexistent_fabro_test.md",
            base.path(),
            &FilesystemFileResolver::new(Some(fallback.path().to_path_buf())),
        )
        .unwrap();
        assert_eq!(result, "@~/nonexistent_fabro_test.md");
    }

    #[test]
    fn resolve_file_ref_fallback_none_behaves_as_before() {
        let base = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_file_ref(
                "@missing.md",
                base.path(),
                &FilesystemFileResolver::new(None)
            )
            .unwrap(),
            "@missing.md"
        );
    }

    #[test]
    fn resolve_file_ref_rejects_template_path() {
        let base = tempfile::tempdir().unwrap();
        let err = resolve_file_ref(
            "@prompts/{{ inputs.prompt_file }}",
            base.path(),
            &FilesystemFileResolver::new(None),
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("templates are not supported in file inline references"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn file_inlining_transform_falls_back_to_fallback_dir() {
        let base = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();
        std::fs::write(fallback.path().join("shared.md"), "shared prompt").unwrap();

        let mut graph = Graph::new("test");
        let mut node = Node::new("work");
        node.attrs.insert(
            "prompt".to_string(),
            AttrValue::String("@shared.md".to_string()),
        );
        graph.nodes.insert("work".to_string(), node);

        let transform = FileInliningTransform::new(
            base.path().to_path_buf(),
            Arc::new(FilesystemFileResolver::new(Some(
                fallback.path().to_path_buf(),
            ))),
        );
        let graph = transform.apply(graph).unwrap();

        assert_eq!(
            graph.nodes["work"]
                .attrs
                .get("prompt")
                .and_then(AttrValue::as_str),
            Some("shared prompt")
        );
    }
}
