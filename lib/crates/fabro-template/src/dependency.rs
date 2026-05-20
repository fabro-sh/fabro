use std::collections::{HashMap, HashSet, VecDeque};

use fabro_types::ManifestPath;
use minijinja::machinery::ast::{Expr, Stmt};
use minijinja::machinery::{self, WhitespaceConfig};
use minijinja::syntax::SyntaxConfig;
use minijinja::value::Value;
use thiserror::Error;

use crate::TemplateError;
use crate::store::{TemplateLoadError, TemplateSource, TemplateStore};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateDependencyKind {
    Include,
    Extends,
    Import,
    FromImport,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateDependency {
    pub kind:      TemplateDependencyKind,
    pub reference: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractedTemplateDependencies {
    pub static_references:  Vec<TemplateDependency>,
    pub dynamic_references: Vec<TemplateDependencyKind>,
}

#[derive(Debug, Error)]
pub enum TemplateDiscoveryError {
    #[error(transparent)]
    Parse(#[from] TemplateError),
    #[error(transparent)]
    Load(#[from] TemplateLoadError),
    #[error("missing template dependency `{reference}` from `{parent}`")]
    Missing {
        parent:    ManifestPath,
        reference: String,
    },
    #[error("dynamic template dependency in `{parent}` must be declared explicitly")]
    Dynamic { parent: ManifestPath },
}

#[derive(Clone, Debug, Default)]
pub struct TemplateDependencyClosure {
    pub sources: HashMap<ManifestPath, TemplateSource>,
}

impl TemplateDependencyClosure {
    #[must_use]
    pub fn paths(&self) -> HashSet<ManifestPath> {
        self.sources.keys().cloned().collect()
    }
}

pub fn extract_template_dependencies(
    source_name: &str,
    source: &str,
) -> Result<ExtractedTemplateDependencies, TemplateError> {
    // MiniJinja's AST is behind `unstable_machinery`; keep direct usage in
    // this module so future MiniJinja upgrades have a single adapter surface.
    let parsed = machinery::parse(
        source,
        source_name,
        SyntaxConfig,
        WhitespaceConfig::default(),
    )
    .map_err(TemplateError::from)?;
    let mut dependencies = ExtractedTemplateDependencies::default();
    collect_stmt_dependencies(&parsed, &mut dependencies);
    Ok(dependencies)
}

pub fn discover_static_dependency_closure(
    roots: impl IntoIterator<Item = TemplateSource>,
    store: &dyn TemplateStore,
) -> Result<TemplateDependencyClosure, TemplateDiscoveryError> {
    let mut sources = HashMap::new();
    let mut queue = VecDeque::new();

    for source in roots {
        if sources
            .insert(source.path.clone(), source.clone())
            .is_none()
        {
            queue.push_back(source);
        }
    }

    while let Some(source) = queue.pop_front() {
        let dependencies =
            extract_template_dependencies(&source.path.to_string(), &source.content)?;
        if !dependencies.dynamic_references.is_empty() {
            return Err(TemplateDiscoveryError::Dynamic {
                parent: source.path,
            });
        }
        for dependency in dependencies.static_references {
            let loaded = store.load(&source, &dependency.reference)?.ok_or_else(|| {
                TemplateDiscoveryError::Missing {
                    parent:    source.path.clone(),
                    reference: dependency.reference.clone(),
                }
            })?;
            if sources
                .insert(loaded.path.clone(), loaded.clone())
                .is_none()
            {
                queue.push_back(loaded);
            }
        }
    }

    Ok(TemplateDependencyClosure { sources })
}

pub(crate) fn has_loader_dependent_tags(
    source_name: &str,
    source: &str,
) -> Result<Option<TemplateDependencyKind>, TemplateError> {
    let dependencies = extract_template_dependencies(source_name, source)?;
    Ok(dependencies
        .static_references
        .first()
        .map(|dependency| dependency.kind)
        .or_else(|| dependencies.dynamic_references.first().copied()))
}

fn collect_stmt_dependencies(stmt: &Stmt<'_>, dependencies: &mut ExtractedTemplateDependencies) {
    match stmt {
        Stmt::Template(template) => collect_stmt_list(&template.children, dependencies),
        Stmt::ForLoop(for_loop) => {
            collect_stmt_list(&for_loop.body, dependencies);
            collect_stmt_list(&for_loop.else_body, dependencies);
        }
        Stmt::IfCond(if_cond) => {
            collect_stmt_list(&if_cond.true_body, dependencies);
            collect_stmt_list(&if_cond.false_body, dependencies);
        }
        Stmt::WithBlock(with_block) => collect_stmt_list(&with_block.body, dependencies),
        Stmt::SetBlock(set_block) => collect_stmt_list(&set_block.body, dependencies),
        Stmt::AutoEscape(auto_escape) => collect_stmt_list(&auto_escape.body, dependencies),
        Stmt::FilterBlock(filter_block) => collect_stmt_list(&filter_block.body, dependencies),
        Stmt::Block(block) => collect_stmt_list(&block.body, dependencies),
        Stmt::Include(include) => {
            collect_loader_expr(TemplateDependencyKind::Include, &include.name, dependencies);
        }
        Stmt::Extends(extends) => {
            collect_loader_expr(TemplateDependencyKind::Extends, &extends.name, dependencies);
        }
        Stmt::Import(import) => {
            collect_loader_expr(TemplateDependencyKind::Import, &import.expr, dependencies);
        }
        Stmt::FromImport(from_import) => collect_loader_expr(
            TemplateDependencyKind::FromImport,
            &from_import.expr,
            dependencies,
        ),
        Stmt::Macro(macro_) => collect_stmt_list(&macro_.body, dependencies),
        Stmt::CallBlock(call_block) => collect_stmt_list(&call_block.macro_decl.body, dependencies),
        Stmt::EmitExpr(_) | Stmt::EmitRaw(_) | Stmt::Set(_) | Stmt::Do(_) => {}
    }
}

fn collect_stmt_list(stmts: &[Stmt<'_>], dependencies: &mut ExtractedTemplateDependencies) {
    for stmt in stmts {
        collect_stmt_dependencies(stmt, dependencies);
    }
}

fn collect_loader_expr(
    kind: TemplateDependencyKind,
    expr: &Expr<'_>,
    dependencies: &mut ExtractedTemplateDependencies,
) {
    if let Some(reference) = const_string(expr) {
        dependencies
            .static_references
            .push(TemplateDependency { kind, reference });
    } else {
        dependencies.dynamic_references.push(kind);
    }
}

fn const_string(expr: &Expr<'_>) -> Option<String> {
    let value: Value = expr.as_const()?;
    value.as_str().map(ToOwned::to_owned)
}
