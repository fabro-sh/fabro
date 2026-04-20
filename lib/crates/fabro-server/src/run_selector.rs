use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolveRunError {
    InvalidSelector,
    AmbiguousPrefix {
        selector: String,
        matches:  Vec<String>,
    },
    NotFound {
        selector: String,
    },
}

impl fmt::Display for ResolveRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSelector => write!(f, "Run selector must not be empty."),
            Self::AmbiguousPrefix { selector, matches } => write!(
                f,
                "Ambiguous prefix '{selector}': {} runs match: {}",
                matches.len(),
                matches.join(", ")
            ),
            Self::NotFound { selector } => write!(
                f,
                "No run found matching '{selector}' (tried run ID prefix and workflow name)"
            ),
        }
    }
}

pub(crate) fn resolve_run_by_selector<'a, T, FRunId, FWorkflowSlug, FWorkflowName, FCreatedAt, K>(
    runs: &'a [T],
    selector: &str,
    run_id: FRunId,
    workflow_slug: FWorkflowSlug,
    workflow_name: FWorkflowName,
    created_at: FCreatedAt,
) -> Result<&'a T, ResolveRunError>
where
    FRunId: Fn(&T) -> String,
    FWorkflowSlug: Fn(&T) -> Option<String>,
    FWorkflowName: Fn(&T) -> Option<String>,
    FCreatedAt: Fn(&T) -> K,
    K: Ord,
{
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(ResolveRunError::InvalidSelector);
    }

    let id_matches: Vec<_> = runs
        .iter()
        .filter(|run| run_id(run).starts_with(selector))
        .collect();
    match id_matches.len() {
        1 => return Ok(id_matches[0]),
        count if count > 1 => {
            return Err(ResolveRunError::AmbiguousPrefix {
                selector: selector.to_string(),
                matches:  id_matches.iter().map(|run| run_id(run)).collect(),
            });
        }
        _ => {}
    }

    let selector_lower = selector.to_lowercase();
    let selector_collapsed = collapse_separators(&selector_lower);
    runs.iter()
        .filter(|run| {
            workflow_slug(run).is_some_and(|slug| slug.to_lowercase() == selector_lower)
                || workflow_name(run).is_some_and(|name| {
                    let name_lower = name.to_lowercase();
                    name_lower.contains(&selector_lower)
                        || collapse_separators(&name_lower).contains(&selector_collapsed)
                })
        })
        .max_by_key(|run| created_at(run))
        .ok_or_else(|| ResolveRunError::NotFound {
            selector: selector.to_string(),
        })
}

fn collapse_separators(value: &str) -> String {
    value.chars().filter(|c| *c != '-' && *c != '_').collect()
}
