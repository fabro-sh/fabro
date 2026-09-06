#![expect(
    clippy::result_large_err,
    reason = "the public error contract preserves concrete config and collection source errors"
)]

use std::path::{Path, PathBuf};

use fabro_config::project::{WorkflowLocation, discover_project_config};
use thiserror::Error;

use crate::workflow_version_collector::{
    canonicalize_location, collect_workflow_versions_at_location,
};
use crate::{CollectedWorkflowClosure, WorkflowVersionCollectError};

/// One local workflow resolved to a canonical package root and collected
/// exactly once.
#[derive(Debug)]
pub struct ResolvedLocalWorkflowPackage {
    workflow_location: WorkflowLocation,
    source_root:       PathBuf,
    closure:           CollectedWorkflowClosure,
}

/// Failure while resolving and collecting one local workflow package.
#[derive(Debug, Error)]
pub enum LocalWorkflowPackageError {
    #[error("failed to resolve local workflow `{workflow}`")]
    Resolve {
        workflow: PathBuf,
        #[source]
        source:   fabro_config::Error,
    },
    #[error("failed to inspect a Git worktree for local workflow `{workflow}`")]
    Repository {
        workflow: PathBuf,
        #[source]
        source:   git2::Error,
    },
    #[error("failed to canonicalize local workflow package path `{path}`")]
    Canonicalize {
        path:   PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to collect local workflow package `{workflow}`")]
    Collect {
        workflow: PathBuf,
        #[source]
        source:   WorkflowVersionCollectError,
    },
}

impl ResolvedLocalWorkflowPackage {
    #[must_use]
    pub fn workflow_location(&self) -> &WorkflowLocation {
        &self.workflow_location
    }

    #[must_use]
    pub fn source_root(&self) -> &Path {
        &self.source_root
    }

    #[must_use]
    pub fn closure(&self) -> &CollectedWorkflowClosure {
        &self.closure
    }

    #[must_use]
    pub fn into_closure(self) -> CollectedWorkflowClosure {
        self.closure
    }
}

/// Resolve producer-readable workflow bytes under one stable local source
/// root, then collect one immutable closure.
///
/// Named workflows prefer the current checkout's `.fabro/workflows` tree,
/// preserve marked-project discovery, and use `user_workflows_root` only when
/// it is explicitly supplied. Explicit paths use their containing Git
/// worktree, the supplied user root, or their own containing directory, in
/// that order. No ambient home or process-current-directory state is read.
pub fn resolve_local_workflow_package(
    workflow: &Path,
    cwd: &Path,
    user_workflows_root: Option<&Path>,
) -> Result<ResolvedLocalWorkflowPackage, LocalWorkflowPackageError> {
    let (location, source_root) = if is_workflow_name(workflow) {
        resolve_named_workflow(workflow, cwd, user_workflows_root)?
    } else {
        resolve_explicit_workflow(workflow, cwd, user_workflows_root)?
    };
    let source_root = canonicalize(&source_root)?;
    let workflow_location = canonicalize_location(location, |path, source| {
        LocalWorkflowPackageError::Canonicalize {
            path: path.to_path_buf(),
            source,
        }
    })?;
    let closure = collect_workflow_versions_at_location(&workflow_location, &source_root, workflow)
        .map_err(|source| LocalWorkflowPackageError::Collect {
            workflow: workflow.to_path_buf(),
            source,
        })?;

    Ok(ResolvedLocalWorkflowPackage {
        workflow_location,
        source_root,
        closure,
    })
}

fn is_workflow_name(workflow: &Path) -> bool {
    workflow.extension().is_none()
        && workflow
            .file_name()
            .is_some_and(|name| workflow.as_os_str() == name)
}

fn resolve_named_workflow(
    workflow: &Path,
    cwd: &Path,
    user_workflows_root: Option<&Path>,
) -> Result<(WorkflowLocation, PathBuf), LocalWorkflowPackageError> {
    let current_root = match worktree_root(cwd, workflow)? {
        Some(root) => root,
        None => canonicalize(cwd)?,
    };
    let project_candidate = current_root
        .join(".fabro/workflows")
        .join(workflow)
        .join("workflow.toml");
    if project_candidate.is_file() {
        return resolve_at(workflow, &project_candidate, current_root);
    }

    let marked_project =
        discover_project_config(cwd).map_err(|source| LocalWorkflowPackageError::Resolve {
            workflow: workflow.to_path_buf(),
            source,
        })?;
    if let Some(config) = marked_project {
        let fabro_root = config
            .parent()
            .expect("a discovered project config has a parent");
        let candidate = fabro_root
            .join("workflows")
            .join(workflow)
            .join("workflow.toml");
        if candidate.is_file() {
            let project_root = fabro_root
                .parent()
                .expect("the .fabro directory has a project parent");
            let source_root = match worktree_root(&candidate, workflow)? {
                Some(root) => root,
                None => canonicalize(project_root)?,
            };
            return resolve_at(workflow, &candidate, source_root);
        }
    }

    if let Some(user_root) = user_workflows_root {
        let candidate = user_root.join(workflow).join("workflow.toml");
        if candidate.is_file() {
            return resolve_at(workflow, &candidate, canonicalize(user_root)?);
        }
    }

    Err(LocalWorkflowPackageError::Resolve {
        workflow: workflow.to_path_buf(),
        source:   fabro_config::Error::WorkflowNotFound(workflow.display().to_string()),
    })
}

fn resolve_explicit_workflow(
    workflow: &Path,
    cwd: &Path,
    user_workflows_root: Option<&Path>,
) -> Result<(WorkflowLocation, PathBuf), LocalWorkflowPackageError> {
    let selected = if workflow.extension().is_none() {
        let path = if workflow.is_absolute() {
            workflow.to_path_buf()
        } else {
            cwd.join(workflow)
        };
        if path.is_dir() {
            path.join("workflow.toml")
        } else {
            return Err(LocalWorkflowPackageError::Resolve {
                workflow: workflow.to_path_buf(),
                source:   fabro_config::Error::WorkflowNotFound(workflow.display().to_string()),
            });
        }
    } else {
        workflow.to_path_buf()
    };
    let location = resolve_location(workflow, &selected, cwd)?;
    if let Some(root) = worktree_root(&location.graph, workflow)? {
        return Ok((location, root));
    }

    if let Some(user_root) = user_workflows_root.filter(|root| root.exists()) {
        let canonical_user_root = canonicalize(user_root)?;
        let canonical_graph = canonicalize(&location.graph)?;
        if canonical_graph.starts_with(&canonical_user_root) {
            return Ok((location, canonical_user_root));
        }
    }

    let source_root = location.dir.clone();
    Ok((location, source_root))
}

fn resolve_at(
    workflow: &Path,
    selected: &Path,
    source_root: PathBuf,
) -> Result<(WorkflowLocation, PathBuf), LocalWorkflowPackageError> {
    let selected = if selected.is_absolute() {
        selected.to_path_buf()
    } else {
        canonicalize(selected)?
    };
    let resolve_from = selected.parent().unwrap_or_else(|| Path::new("."));
    resolve_location(workflow, &selected, resolve_from).map(|location| (location, source_root))
}

fn resolve_location(
    workflow: &Path,
    selected: &Path,
    cwd: &Path,
) -> Result<WorkflowLocation, LocalWorkflowPackageError> {
    crate::resolve_existing_workflow_location(selected, cwd).map_err(|source| {
        LocalWorkflowPackageError::Resolve {
            workflow: workflow.to_path_buf(),
            source,
        }
    })
}

fn worktree_root(
    path: &Path,
    workflow: &Path,
) -> Result<Option<PathBuf>, LocalWorkflowPackageError> {
    let discover_from = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let repository = match git2::Repository::discover(discover_from) {
        Ok(repository) => repository,
        Err(source) if source.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(source) => {
            return Err(LocalWorkflowPackageError::Repository {
                workflow: workflow.to_path_buf(),
                source,
            });
        }
    };
    if repository.is_bare() {
        return Ok(None);
    }
    Ok(repository.workdir().map(Path::to_path_buf))
}

fn canonicalize(path: &Path) -> Result<PathBuf, LocalWorkflowPackageError> {
    path.canonicalize()
        .map_err(|source| LocalWorkflowPackageError::Canonicalize {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::disallowed_methods,
        reason = "local-package tests build isolated filesystem and Git fixtures synchronously"
    )]

    use std::fs;
    use std::path::{Path, PathBuf};

    use fabro_types::WorkflowVersion;
    use fabro_util::error::collect_chain;

    use crate::{LocalWorkflowPackageError, resolve_local_workflow_package};

    fn write(root: &Path, path: &str, content: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn write_workflow(root: &Path, directory: &str, graph: &str) -> PathBuf {
        write(
            root,
            &format!("{directory}/workflow.toml"),
            "_version = 1\n[workflow]\ngraph = \"workflow.fabro\"\n",
        );
        write(root, &format!("{directory}/workflow.fabro"), graph);
        root.join(directory).join("workflow.toml")
    }

    fn init_repo(path: &Path) {
        git2::Repository::init(path).unwrap();
    }

    fn canonical_versions(
        package: &crate::ResolvedLocalWorkflowPackage,
    ) -> Vec<(fabro_types::WorkflowVersionId, Vec<u8>)> {
        package
            .closure()
            .versions()
            .map(|(id, version)| (id, version.version().canonical_bytes().unwrap()))
            .collect()
    }

    fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
        collect_chain(error).join(": ")
    }

    fn root_version(package: &crate::ResolvedLocalWorkflowPackage) -> &WorkflowVersion {
        package.closure().versions().last().unwrap().1.version()
    }

    #[test]
    fn named_markerless_project_workflow_precedes_explicit_user_root() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let user = temp.path().join("user-workflows");
        fs::create_dir_all(&project).unwrap();
        init_repo(&project);
        write_workflow(&project, ".fabro/workflows/hello", "digraph Project {}");
        write_workflow(&user, "hello", "digraph User {}");

        let package =
            resolve_local_workflow_package(Path::new("hello"), &project, Some(&user)).unwrap();

        assert_eq!(package.source_root(), project.canonicalize().unwrap());
        assert_eq!(
            package.workflow_location().graph,
            project
                .join(".fabro/workflows/hello/workflow.fabro")
                .canonicalize()
                .unwrap(),
        );
        assert_eq!(
            root_version(&package).entrypoint().as_str(),
            ".fabro/workflows/hello/workflow.fabro",
        );
    }

    #[test]
    fn named_user_workflow_requires_and_uses_the_explicit_root() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("cwd");
        let user = temp.path().join("user-workflows");
        fs::create_dir_all(&cwd).unwrap();
        write_workflow(&user, "hello", "digraph User {}");

        let package =
            resolve_local_workflow_package(Path::new("hello"), &cwd, Some(&user)).unwrap();
        assert_eq!(package.source_root(), user.canonicalize().unwrap());
        assert_eq!(
            root_version(&package).entrypoint().as_str(),
            "hello/workflow.fabro",
        );

        let error = resolve_local_workflow_package(Path::new("hello"), &cwd, None).unwrap_err();
        assert!(matches!(error, LocalWorkflowPackageError::Resolve { .. }));
    }

    #[test]
    fn dot_relative_directory_is_an_explicit_path_even_when_name_exists() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        init_repo(&project);
        write_workflow(&project, ".fabro/workflows/hello", "digraph Named {}");
        write_workflow(&project, "hello", "digraph Explicit {}");

        let package = resolve_local_workflow_package(Path::new("./hello"), &project, None).unwrap();

        assert_eq!(package.source_root(), project.canonicalize().unwrap());
        assert_eq!(
            package.workflow_location().graph,
            project.join("hello/workflow.fabro").canonicalize().unwrap(),
        );
        assert_eq!(
            root_version(&package).entrypoint().as_str(),
            "hello/workflow.fabro",
        );
    }

    #[test]
    fn explicit_workflow_uses_its_own_checkout_and_has_location_independent_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let caller = temp.path().join("caller");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        for root in [&caller, &first, &second] {
            fs::create_dir_all(root).unwrap();
            init_repo(root);
        }
        let first_workflow = write_workflow(
            &first,
            "flows/demo",
            "digraph Demo { task [prompt=\"@prompt.md\"] }",
        );
        write(&first, "flows/demo/prompt.md", "hello");
        let second_workflow = write_workflow(
            &second,
            "flows/demo",
            "digraph Demo { task [prompt=\"@prompt.md\"] }",
        );
        write(&second, "flows/demo/prompt.md", "hello");

        let first_package = resolve_local_workflow_package(&first_workflow, &caller, None).unwrap();
        let second_package =
            resolve_local_workflow_package(&second_workflow, &caller, None).unwrap();

        assert_eq!(first_package.source_root(), first.canonicalize().unwrap());
        assert_eq!(second_package.source_root(), second.canonicalize().unwrap());
        assert_eq!(
            canonical_versions(&first_package),
            canonical_versions(&second_package)
        );
    }

    #[test]
    fn workflow_in_a_checkout_allows_parent_segments_that_stay_inside_the_root() {
        let temp = tempfile::tempdir().unwrap();
        let package_root = temp.path().join("package");
        fs::create_dir_all(&package_root).unwrap();
        init_repo(&package_root);
        let workflow = write_workflow(
            &package_root,
            "flows",
            "digraph Demo { task [prompt=\"@../shared.md\"] }",
        );
        write(&package_root, "shared.md", "shared");

        let package = resolve_local_workflow_package(&workflow, temp.path(), None).unwrap();

        assert_eq!(package.source_root(), package_root.canonicalize().unwrap());
        assert!(
            root_version(&package)
                .files()
                .keys()
                .any(|path| path.as_str() == "shared.md"),
        );
    }

    #[test]
    fn loose_workflow_uses_its_canonical_containing_directory() {
        let temp = tempfile::tempdir().unwrap();
        let workflow = write_workflow(
            temp.path(),
            "loose",
            "digraph Demo { task [prompt=\"@prompt.md\"] }",
        );
        write(temp.path(), "loose/prompt.md", "hello");

        let package = resolve_local_workflow_package(&workflow, temp.path(), None).unwrap();

        assert_eq!(
            package.source_root(),
            temp.path().join("loose").canonicalize().unwrap(),
        );
        assert!(
            root_version(&package)
                .files()
                .keys()
                .any(|path| path.as_str() == "prompt.md")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_direct_and_template_symlinks_that_escape_a_loose_package() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        write(&outside, "secret.md", "secret");

        let direct = write_workflow(
            temp.path(),
            "direct",
            "digraph Demo { task [prompt=\"@secret.md\"] }",
        );
        symlink(
            outside.join("secret.md"),
            temp.path().join("direct/secret.md"),
        )
        .unwrap();
        let direct_error = resolve_local_workflow_package(&direct, temp.path(), None).unwrap_err();
        let direct_chain = error_chain(&direct_error);
        assert!(direct_chain.contains("secret.md"), "{direct_chain}");
        assert!(direct_chain.contains("direct"), "{direct_chain}");

        let template = write_workflow(
            temp.path(),
            "template",
            "digraph Demo { task [prompt=\"@prompt.md\"] }",
        );
        write(
            temp.path(),
            "template/prompt.md",
            "{% include \"secret.md\" %}",
        );
        symlink(
            outside.join("secret.md"),
            temp.path().join("template/secret.md"),
        )
        .unwrap();
        let template_error =
            resolve_local_workflow_package(&template, temp.path(), None).unwrap_err();
        let template_chain = error_chain(&template_error);
        assert!(
            template_chain.contains("escapes template root"),
            "{template_chain}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_selected_workflow_symlink_outside_its_checkout() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let outside = temp.path().join("outside");
        fs::create_dir_all(project.join(".fabro/workflows/hello")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        init_repo(&project);
        write(
            &project,
            ".fabro/workflows/hello/workflow.toml",
            "_version = 1\n[workflow]\ngraph = \"workflow.fabro\"\n",
        );
        write(&outside, "workflow.fabro", "digraph Outside {}");
        symlink(
            outside.join("workflow.fabro"),
            project.join(".fabro/workflows/hello/workflow.fabro"),
        )
        .unwrap();

        let error = resolve_local_workflow_package(Path::new("hello"), &project, None).unwrap_err();

        assert!(matches!(error, LocalWorkflowPackageError::Collect { .. }));
        assert!(error_chain(&error).contains("escapes source root"));
    }

    #[test]
    fn malformed_and_missing_workflows_preserve_config_sources() {
        let temp = tempfile::tempdir().unwrap();
        let malformed = temp.path().join("malformed/workflow.toml");
        write(temp.path(), "malformed/workflow.toml", "not valid = [");

        let malformed_error =
            resolve_local_workflow_package(&malformed, temp.path(), None).unwrap_err();
        assert!(matches!(
            malformed_error,
            LocalWorkflowPackageError::Resolve { .. }
        ));
        assert!(std::error::Error::source(&malformed_error).is_some());

        let missing =
            resolve_local_workflow_package(Path::new("missing"), temp.path(), None).unwrap_err();
        assert!(matches!(missing, LocalWorkflowPackageError::Resolve { .. }));
        assert!(std::error::Error::source(&missing).is_some());
    }
}
