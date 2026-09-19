use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use fabro_api::types;
use fabro_config::project::WorkflowLocation;
use fabro_config::{
    EnvironmentDockerfileLayer, EnvironmentImageLayer, RunGoalLayer, SettingsLayer,
};
use fabro_graphviz::parser;
use fabro_template::{
    BundleTemplateStore, FilesystemTemplateStore, GraphPosition, GraphReference,
    GraphReferenceError, RecordingTemplateStore, TemplateContext, TemplateDependencyClosure,
    TemplateRenderMode, TemplateSource, validate_static_reference, visit_graph_references,
};
use fabro_types::ManifestPath;
use fabro_types::graph::ReferenceKind;

use crate::{
    WorkflowVersionCollectError, manifest_path_from_absolute, normalize_absolute_path,
    workflow_version_collector,
};

pub(super) struct WorkflowBundler<'a> {
    package_root: &'a Path,
    inputs: &'a HashMap<String, toml::Value>,
    template_store: FilesystemTemplateStore,
    workflows: HashMap<String, CollectedWorkflowSource>,
    visited_workflows: HashSet<String>,
    workflow_version_projection: bool,
}

pub(super) struct CollectedWorkflowSources {
    pub(super) root_key:  String,
    pub(super) workflows: HashMap<String, CollectedWorkflowSource>,
}

pub(super) struct CollectedWorkflowSource {
    pub(super) workflow:        types::ManifestWorkflow,
    pub(super) dependency_keys: BTreeSet<String>,
}

impl<'a> WorkflowBundler<'a> {
    pub(super) fn new(package_root: &'a Path, inputs: &'a HashMap<String, toml::Value>) -> Self {
        Self {
            package_root,
            inputs,
            template_store: FilesystemTemplateStore::new(package_root),
            workflows: HashMap::new(),
            visited_workflows: HashSet::new(),
            workflow_version_projection: false,
        }
    }

    pub(super) fn bundle(
        mut self,
        workflow: &Path,
        project_config: Option<(&ManifestPath, &str)>,
    ) -> Result<HashMap<String, types::ManifestWorkflow>> {
        let root_key = self.collect_workflow_entry(workflow, self.package_root, 1)?;

        if let Some((config_path, source)) = project_config {
            let mut root = self
                .workflows
                .remove(&root_key)
                .ok_or_else(|| anyhow!("root workflow missing from manifest bundle"))?;
            let entrypoint = ManifestPath::from_wire(&root_key)
                .ok_or_else(|| anyhow!("invalid root workflow path: {root_key}"))?;
            self.collect_config_files(config_path, source, &entrypoint, &mut root.workflow.files)?;
            self.workflows.insert(root_key, root);
        }

        Ok(self
            .workflows
            .into_iter()
            .map(|(key, source)| (key, source.workflow))
            .collect())
    }

    pub(super) fn collect_versions(
        mut self,
        root: &WorkflowLocation,
    ) -> Result<CollectedWorkflowSources> {
        self.workflow_version_projection = true;
        let root_key = self.collect_workflow_location(root, 1)?;
        Ok(CollectedWorkflowSources {
            root_key,
            workflows: self.workflows,
        })
    }

    /// Collects the workflow at `location` and returns its manifest key.
    fn collect_workflow_location(
        &mut self,
        location: &WorkflowLocation,
        depth: usize,
    ) -> Result<String> {
        let dot_path = manifest_path_from_absolute(&location.graph, self.package_root)?;
        let dot_key = dot_path.to_string();
        if !self.visited_workflows.insert(dot_key.clone()) {
            return Ok(dot_key);
        }
        if self.workflow_version_projection {
            workflow_version_collector::check_workflow_depth(depth, &dot_key)?;
        }

        let source = self.read_package_file(&location.graph)?;
        let config = if let Some(workflow_toml_path) = location.toml.as_ref() {
            let config_path = manifest_path_from_absolute(workflow_toml_path, self.package_root)?;
            if self.workflow_version_projection {
                // A version's config is read at run time from the fixed
                // sibling path only, so a config file under any other name
                // would be registered and then silently ignored.
                let expected = dot_path.parent_or_dot().join("workflow.toml");
                if config_path.as_path() != expected {
                    bail!(
                        "workflow configuration `{config_path}` must be `{}` beside its graph \
                         `{dot_path}`",
                        expected.display()
                    );
                }
            }
            Some(types::ManifestWorkflowConfig {
                path:   config_path.to_string(),
                source: self.read_package_file(workflow_toml_path)?,
            })
        } else {
            None
        };

        let scan = WorkflowScanInput {
            absolute_dot_path: location.graph.clone(),
            dot_path:          dot_path.clone(),
            source:            source.clone(),
        };
        let mut files = HashMap::new();
        let mut visited_imports = HashSet::new();
        let mut dependency_keys = BTreeSet::new();
        if let Some(config) = config.as_ref() {
            let config_path = ManifestPath::from_wire(&config.path)
                .ok_or_else(|| anyhow!("invalid manifest workflow config path: {}", config.path))?;
            self.collect_config_files(&config_path, &config.source, &dot_path, &mut files)?;
        }
        self.collect_workflow_files(
            &scan,
            &mut files,
            &mut visited_imports,
            &mut dependency_keys,
            GraphPosition::Entrypoint,
            depth,
        )?;

        self.workflows
            .insert(dot_key.clone(), CollectedWorkflowSource {
                workflow: types::ManifestWorkflow {
                    config,
                    files,
                    source,
                },
                dependency_keys,
            });

        Ok(dot_key)
    }

    /// Relative workflow references with an extension are lexically
    /// normalized (`..` segments resolved without consulting the filesystem,
    /// `~` rejected) before resolution, so the file read matches the manifest
    /// key. Workflow-version projection normalizes every reference and
    /// resolves it as an exact path inside the package root, with no
    /// workflow-name lookup. Returns the collected workflow's manifest key.
    fn collect_workflow_entry(
        &mut self,
        workflow: &Path,
        resolve_from: &Path,
        depth: usize,
    ) -> Result<String> {
        let normalize = self.workflow_version_projection
            || (workflow.extension().is_some() && workflow.is_relative());
        let normalized = if normalize {
            normalize_absolute_path(resolve_from, &workflow.to_string_lossy()).ok_or_else(|| {
                anyhow!(
                    "unsupported manifest workflow reference: {}",
                    workflow.display()
                )
            })?
        } else {
            workflow.to_path_buf()
        };
        let location = if self.workflow_version_projection {
            // Location resolution probes and parses config files, so reject
            // references that leave the package root before it can touch a
            // host file. `ManifestPath::from_absolute` accepts `..`-prefixed
            // results and is not a containment check.
            if !normalized.starts_with(self.package_root) {
                bail!(
                    "workflow reference `{}` escapes source root `{}`",
                    workflow.display(),
                    self.package_root.display()
                );
            }
            WorkflowLocation::from_exact_path(&normalized, self.package_root)?
        } else {
            WorkflowLocation::resolve(&normalized, resolve_from)?
        };
        self.collect_workflow_location(&location, depth)
    }

    fn collect_workflow_files(
        &mut self,
        workflow: &WorkflowScanInput,
        files: &mut HashMap<String, types::ManifestFileEntry>,
        visited_imports: &mut HashSet<String>,
        dependency_keys: &mut BTreeSet<String>,
        position: GraphPosition,
        depth: usize,
    ) -> Result<()> {
        if self.workflow_version_projection {
            workflow_version_collector::check_workflow_depth(
                depth,
                &workflow.dot_path.to_string(),
            )?;
        }
        let graph = parser::parse(&workflow.source)
            .with_context(|| format!("Failed to parse {}", workflow.absolute_dot_path.display()))?;
        let workflow_base_dir = workflow
            .absolute_dot_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let workflow_template_root = if self.workflow_version_projection {
            workflow_package_root()
        } else {
            manifest_parent_or_dot(&workflow.dot_path)?
        };

        // Imports and child workflows require a mutable borrow of self, so
        // collect them during the walk and recurse after the visitor returns.
        let mut imports = Vec::new();
        let mut children = Vec::new();

        visit_graph_references(&graph, position, |reference| -> Result<()> {
            match reference {
                GraphReference::GoalFile { reference } => {
                    let bundled = self.collect_bundled_file(
                        files,
                        workflow_base_dir,
                        reference,
                        types::ManifestFileRefType::FileInline,
                        ReferenceKind::GraphGoalFile,
                        Some(workflow.dot_path.clone()),
                    )?;
                    self.collect_bundled_template_includes(files, &bundled, &workflow_template_root)
                }
                GraphReference::GoalInline { content }
                | GraphReference::InlinePrompt { content }
                | GraphReference::ModelStylesheetInline { content } => self
                    .collect_template_include_files(
                        files,
                        TemplateSource::new(
                            workflow.dot_path.clone(),
                            workflow_template_root.clone(),
                            content.to_owned(),
                        ),
                        Some(&workflow.dot_path),
                    ),
                GraphReference::FileInline { key, reference } => {
                    let bundled = self.collect_bundled_file(
                        files,
                        workflow_base_dir,
                        reference,
                        types::ManifestFileRefType::FileInline,
                        ReferenceKind::FileInline,
                        Some(workflow.dot_path.clone()),
                    )?;
                    if key == "prompt" {
                        self.collect_bundled_template_includes(
                            files,
                            &bundled,
                            &workflow_template_root,
                        )?;
                    }
                    Ok(())
                }
                GraphReference::Import { reference } => {
                    let imported = self.collect_bundled_file(
                        files,
                        workflow_base_dir,
                        reference,
                        types::ManifestFileRefType::Import,
                        ReferenceKind::Import,
                        Some(workflow.dot_path.clone()),
                    )?;
                    imports.push(imported);
                    Ok(())
                }
                GraphReference::ChildWorkflow { reference } => {
                    children.push(reference);
                    Ok(())
                }
            }
        })
        .map_err(|error| match error {
            GraphReferenceError::StaticReference(source) => anyhow::Error::new(source),
            GraphReferenceError::Visit(error) => error,
        })?;

        for imported in imports {
            if visited_imports.insert(imported.path.to_string()) {
                let imported_source = self.read_package_file(&imported.absolute_path)?;
                let imported_scan = WorkflowScanInput {
                    absolute_dot_path: imported.absolute_path,
                    dot_path:          imported.path,
                    source:            imported_source,
                };
                self.collect_workflow_files(
                    &imported_scan,
                    files,
                    visited_imports,
                    dependency_keys,
                    GraphPosition::Imported,
                    depth + 1,
                )?;
            }
        }
        for child in children {
            let dependency_key =
                self.collect_workflow_entry(Path::new(child), workflow_base_dir, depth + 1)?;
            dependency_keys.insert(dependency_key);
        }

        Ok(())
    }

    fn collect_bundled_template_includes(
        &self,
        files: &mut HashMap<String, types::ManifestFileEntry>,
        bundled: &BundledFile,
        workflow_template_root: &ManifestPath,
    ) -> Result<()> {
        let source = self.read_package_file(&bundled.absolute_path)?;
        let template_root = template_root_for_bundled_file(&bundled.path, workflow_template_root)?;
        self.collect_template_include_files(
            files,
            TemplateSource::new(bundled.path.clone(), template_root, source),
            Some(&bundled.path),
        )
    }

    fn collect_template_include_files(
        &self,
        files: &mut HashMap<String, types::ManifestFileEntry>,
        source: TemplateSource,
        from: Option<&ManifestPath>,
    ) -> Result<()> {
        let source_path = source.path.clone();
        let closure =
            fabro_template::discover_static_dependency_closure([source], &self.template_store)
                .context("failed to discover template dependencies")?;
        self.verify_recorded_template_dependencies(&source_path, &closure, files, from)?;

        for (path, source) in closure.sources {
            if path == source_path {
                continue;
            }
            let key = path.to_string();
            files
                .entry(key)
                .or_insert_with(|| types::ManifestFileEntry {
                    content: source.content,
                    ref_:    types::ManifestFileRef {
                        from:     from.map(std::string::ToString::to_string),
                        original: path.to_string(),
                        type_:    types::ManifestFileRefType::FileInline,
                    },
                });
        }
        Ok(())
    }

    fn verify_recorded_template_dependencies(
        &self,
        source_path: &ManifestPath,
        closure: &TemplateDependencyClosure,
        files: &HashMap<String, types::ManifestFileEntry>,
        from: Option<&ManifestPath>,
    ) -> Result<()> {
        let Some(source) = closure.sources.get(source_path) else {
            return Ok(());
        };
        let mut bundled_files = closure
            .sources
            .iter()
            .map(|(path, source)| (path.clone(), source.content.clone()))
            .collect::<HashMap<_, _>>();
        for (path, entry) in files {
            if let Some(path) = ManifestPath::from_wire(path) {
                bundled_files.insert(path, entry.content.clone());
            }
        }
        let allowed = bundled_files.keys().cloned().collect();
        let store =
            RecordingTemplateStore::with_allowed(BundleTemplateStore::new(bundled_files), allowed);
        let context = TemplateContext::for_input_scan(self.inputs.clone());
        fabro_template::render_source(
            source,
            &context,
            Arc::new(store),
            TemplateRenderMode::Lenient,
        )
        .with_context(|| {
            let from =
                from.map_or_else(|| source_path.to_string(), std::string::ToString::to_string);
            format!("failed to verify template dependencies for {from}")
        })?;
        Ok(())
    }

    fn collect_config_files(
        &self,
        config_path: &ManifestPath,
        source: &str,
        entrypoint: &ManifestPath,
        files: &mut HashMap<String, types::ManifestFileEntry>,
    ) -> Result<()> {
        let layer = source
            .parse::<SettingsLayer>()
            .context("Failed to parse run config TOML")?;
        let absolute_config_path = self.package_root.join(config_path.as_path());
        let base_dir = absolute_config_path
            .parent()
            .unwrap_or_else(|| Path::new("."));

        for image in layer.environment_images() {
            self.collect_environment_dockerfile(files, base_dir, config_path, image)?;
        }
        // The goal file rides with the manifest: Petri reads `[run.goal]
        // file` from the bundle at check, as it does for a version.
        self.collect_config_goal_files(files, base_dir, config_path, entrypoint, &layer)?;
        Ok(())
    }

    fn collect_config_goal_files(
        &self,
        files: &mut HashMap<String, types::ManifestFileEntry>,
        base_dir: &Path,
        config_path: &ManifestPath,
        entrypoint: &ManifestPath,
        layer: &SettingsLayer,
    ) -> Result<()> {
        let Some(goal) = layer.run.as_ref().and_then(|run| run.goal.as_ref()) else {
            return Ok(());
        };
        let content = match goal {
            RunGoalLayer::Inline(goal) => goal.as_source(),
            RunGoalLayer::File { file } => {
                let reference = file.as_source();
                let bundled = self.collect_bundled_file(
                    files,
                    base_dir,
                    &reference,
                    types::ManifestFileRefType::FileInline,
                    ReferenceKind::RunGoalFile,
                    Some(config_path.clone()),
                )?;
                files
                    .get(&bundled.path.to_string())
                    .expect("collect_bundled_file inserts the goal file it returns")
                    .content
                    .clone()
            }
        };
        self.collect_template_include_files(
            files,
            TemplateSource::new(entrypoint.clone(), workflow_package_root(), content),
            Some(config_path),
        )
    }

    fn collect_environment_dockerfile(
        &self,
        files: &mut HashMap<String, types::ManifestFileEntry>,
        base_dir: &Path,
        config_path: &ManifestPath,
        image: &EnvironmentImageLayer,
    ) -> Result<()> {
        let Some(EnvironmentDockerfileLayer::Path { path }) = image.dockerfile.as_ref() else {
            return Ok(());
        };
        self.collect_bundled_file(
            files,
            base_dir,
            path,
            types::ManifestFileRefType::Dockerfile,
            ReferenceKind::Dockerfile,
            Some(config_path.clone()),
        )?;
        Ok(())
    }

    fn collect_bundled_file(
        &self,
        files: &mut HashMap<String, types::ManifestFileEntry>,
        base_dir: &Path,
        reference: &str,
        ref_type: types::ManifestFileRefType,
        reference_kind: ReferenceKind,
        from: Option<ManifestPath>,
    ) -> Result<BundledFile> {
        validate_static_reference(reference, reference_kind).map_err(anyhow::Error::new)?;

        let absolute_path = normalize_absolute_path(base_dir, reference)
            .ok_or_else(|| anyhow!("unsupported manifest reference: {reference}"))?;
        let path = manifest_path_from_absolute(&absolute_path, self.package_root)?;
        let key = path.to_string();
        if !files.contains_key(&key) {
            let content = self.read_package_file(&absolute_path)?;
            files.insert(key.clone(), types::ManifestFileEntry {
                content,
                ref_: types::ManifestFileRef {
                    from:     from.map(|value| value.to_string()),
                    original: reference.to_owned(),
                    type_:    ref_type,
                },
            });
        }

        Ok(BundledFile {
            absolute_path,
            path,
        })
    }

    fn read_package_file(&self, path: &Path) -> Result<String> {
        if !self.workflow_version_projection {
            return std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read {}", path.display()));
        }
        let canonical = path.canonicalize().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                let path = ManifestPath::from_absolute(path, self.package_root)
                    .map_or_else(|| path.display().to_string(), |path| path.to_string());
                return anyhow::Error::new(WorkflowVersionCollectError::MissingPackageFile {
                    path,
                });
            }
            anyhow::Error::new(source).context(format!(
                "failed to canonicalize workflow package file `{}`",
                path.display()
            ))
        })?;
        if !canonical.starts_with(self.package_root) {
            bail!(
                "workflow package file `{}` escapes source root `{}`",
                path.display(),
                self.package_root.display()
            );
        }
        std::fs::read_to_string(&canonical).with_context(|| {
            format!(
                "failed to read workflow package file `{}`",
                canonical.display()
            )
        })
    }
}

struct WorkflowScanInput {
    absolute_dot_path: PathBuf,
    dot_path:          ManifestPath,
    source:            String,
}

struct BundledFile {
    absolute_path: PathBuf,
    path:          ManifestPath,
}

fn manifest_parent_or_dot(path: &ManifestPath) -> Result<ManifestPath> {
    let parent = path.parent_or_dot().to_string_lossy();
    ManifestPath::from_wire(&parent)
        .ok_or_else(|| anyhow!("invalid manifest parent path for {path}: {parent}"))
}

fn workflow_package_root() -> ManifestPath {
    ManifestPath::from_wire(".").expect("the workflow package root must be a valid manifest path")
}

fn template_root_for_bundled_file(
    path: &ManifestPath,
    workflow_template_root: &ManifestPath,
) -> Result<ManifestPath> {
    if manifest_path_is_within_root(path, workflow_template_root) {
        Ok(workflow_template_root.clone())
    } else {
        manifest_parent_or_dot(path)
    }
}

fn manifest_path_is_within_root(path: &ManifestPath, root: &ManifestPath) -> bool {
    if root.as_path().as_os_str().is_empty() {
        return !matches!(
            path.as_path().components().next(),
            Some(Component::ParentDir)
        );
    }
    path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::write_file;

    fn bundle_graph(cwd: &Path, graph: &Path) -> Result<HashMap<String, types::ManifestWorkflow>> {
        let inputs = HashMap::new();
        WorkflowBundler::new(cwd, &inputs).bundle(graph, None)
    }

    #[test]
    fn repeated_references_collect_one_file() {
        let temp = tempfile::tempdir().expect("temp directory should be created");
        let graph = temp.path().join("workflow.fabro");
        write_file(
            &graph,
            r#"digraph Root {
                start [shape=Mdiamond]
                first [prompt="@prompt.md"]
                second [prompt="@prompt.md"]
                exit [shape=Msquare]
                start -> first -> second -> exit
            }"#,
        );
        write_file(&temp.path().join("prompt.md"), "prompt\n");

        let workflows = bundle_graph(temp.path(), &graph).expect("workflow should bundle");

        assert_eq!(workflows["workflow.fabro"].files.len(), 1);
    }

    #[test]
    fn graph_goal_bundles_filename_with_at_prefix() {
        let temp = tempfile::tempdir().expect("temp directory should be created");
        let graph = temp.path().join("workflow.fabro");
        write_file(
            &graph,
            r#"digraph Root {
                graph [goal="@@goal.md"]
                start [shape=Mdiamond]
                exit [shape=Msquare]
                start -> exit
            }"#,
        );
        write_file(&temp.path().join("@goal.md"), "goal\n");

        let workflows = bundle_graph(temp.path(), &graph).expect("workflow should bundle");

        let goal = &workflows["workflow.fabro"].files["@goal.md"];
        assert_eq!(goal.content, "goal\n");
        assert_eq!(goal.ref_.original, "@goal.md");
    }

    #[test]
    fn root_model_stylesheet_bundles_nested_static_includes() {
        let temp = tempfile::tempdir().expect("temp directory should be created");
        let graph = temp.path().join("workflow.fabro");
        write_file(
            &graph,
            r#"digraph Root {
                graph [model_stylesheet="{% include 'styles/base.css' %}"]
                start [shape=Mdiamond]
                exit [shape=Msquare]
                start -> exit
            }"#,
        );
        write_file(
            &temp.path().join("styles/base.css"),
            "{% include 'nested.css' %}",
        );
        write_file(
            &temp.path().join("styles/nested.css"),
            "* { reasoning_effort: low; }",
        );

        let workflows = bundle_graph(temp.path(), &graph).expect("workflow should bundle");
        let files = &workflows["workflow.fabro"].files;

        assert_eq!(
            files["styles/base.css"].content,
            "{% include 'nested.css' %}"
        );
        assert_eq!(
            files["styles/nested.css"].content,
            "* { reasoning_effort: low; }"
        );
    }

    #[test]
    fn root_model_stylesheet_rejects_invalid_includes() {
        for template in [
            "{% include 'missing.css' %}",
            "{% include inputs.stylesheet %}",
            "{% include '../outside.css' %}",
        ] {
            let temp = tempfile::tempdir().expect("temp directory should be created");
            let graph = temp.path().join("workflow.fabro");
            write_file(
                &graph,
                &format!(
                    r#"digraph Root {{
                        graph [model_stylesheet="{template}"]
                        start [shape=Mdiamond]
                        exit [shape=Msquare]
                        start -> exit
                    }}"#,
                ),
            );

            let error = bundle_graph(temp.path(), &graph)
                .expect_err("invalid stylesheet include should fail bundling");
            assert!(
                error.to_string().contains("template dependencies"),
                "template: {template}; error: {error:#}"
            );
        }
    }

    #[test]
    fn imported_model_stylesheet_includes_are_not_bundled() {
        let temp = tempfile::tempdir().expect("temp directory should be created");
        let graph = temp.path().join("workflow.fabro");
        write_file(
            &graph,
            r#"digraph Root {
                start [shape=Mdiamond]
                child [import="child.fabro"]
                exit [shape=Msquare]
                start -> child -> exit
            }"#,
        );
        write_file(
            &temp.path().join("child.fabro"),
            r#"digraph Child {
                graph [model_stylesheet="{% include 'missing.css' %}"]
                start [shape=Mdiamond]
                exit [shape=Msquare]
                start -> exit
            }"#,
        );

        let workflows = bundle_graph(temp.path(), &graph).expect("workflow should bundle");
        let files = &workflows["workflow.fabro"].files;

        assert!(files.contains_key("child.fabro"));
        assert!(!files.contains_key("missing.css"));
    }

    #[test]
    fn parse_errors_keep_the_graphviz_error_in_the_source_chain() {
        let temp = tempfile::tempdir().expect("temp directory should be created");
        let graph = temp.path().join("workflow.fabro");
        write_file(&graph, "not a graph");

        let error = bundle_graph(temp.path(), &graph).expect_err("invalid graph should fail");

        assert!(
            error
                .chain()
                .any(|cause| cause.downcast_ref::<fabro_graphviz::Error>().is_some()),
            "unexpected error chain: {error:#}"
        );
    }

    #[test]
    fn read_errors_keep_the_io_error_in_the_source_chain() {
        let temp = tempfile::tempdir().expect("temp directory should be created");
        let graph = temp.path().join("workflow.fabro");
        write_file(
            &graph,
            r#"digraph Root {
                start [shape=Mdiamond]
                work [prompt="@missing.md"]
                exit [shape=Msquare]
                start -> work -> exit
            }"#,
        );

        let error = bundle_graph(temp.path(), &graph).expect_err("missing file should fail");

        assert!(
            error
                .chain()
                .any(|cause| cause.downcast_ref::<std::io::Error>().is_some()),
            "unexpected error chain: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn root_workflow_normalizes_parent_components_lexically_before_reading() {
        let temp = tempfile::tempdir().expect("temp directory should be created");
        let cwd = temp.path();
        let lexical_graph =
            "digraph Lexical { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }";
        let symlinked_graph =
            "digraph Symlinked { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }";
        write_file(&cwd.join("wf/workflow.fabro"), lexical_graph);
        write_file(&cwd.join("nested/wf/workflow.fabro"), symlinked_graph);
        std::fs::create_dir_all(cwd.join("nested/elsewhere"))
            .expect("symlink target should be created");
        // `link` points into `nested/`, so OS resolution of `link/..` lands in
        // `nested/` while lexical resolution lands in the invocation directory.
        std::os::unix::fs::symlink(cwd.join("nested/elsewhere"), cwd.join("link"))
            .expect("symlink should be created");

        let workflows = bundle_graph(cwd, Path::new("link/../wf/workflow.fabro"))
            .expect("workflow should bundle");

        // `link/..` must resolve lexically to `wf/workflow.fabro`, not through
        // the symlink to `nested/wf/workflow.fabro`, so the bundled source
        // matches the file the manifest key names.
        assert_eq!(workflows["wf/workflow.fabro"].source, lexical_graph);
    }

    #[test]
    fn root_workflow_rejects_tilde_relative_references() {
        let temp = tempfile::tempdir().expect("temp directory should be created");

        let error = bundle_graph(temp.path(), Path::new("~/workflow.fabro"))
            .expect_err("tilde reference should be rejected");

        assert!(
            error
                .to_string()
                .contains("unsupported manifest workflow reference"),
            "unexpected error: {error:#}"
        );
    }
}
