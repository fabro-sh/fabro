use fabro_util::env::Env;

/// Context for variable substitution.
pub(crate) struct VariableContext<'a> {
    pub local_workspace_folder: String,
    pub local_workspace_folder_basename: String,
    pub container_workspace_folder: String,
    pub env: &'a dyn Env,
}

/// Replace devcontainer variables in a string value.
pub(crate) fn substitute(input: &str, ctx: &VariableContext) -> String {
    let mut result = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];

        if let Some(close) = after_open.find('}') {
            let expr = &after_open[..close];
            let replacement = resolve_variable(expr, ctx);
            match replacement {
                Some(val) => result.push_str(&val),
                None => {
                    // Unknown variable — leave as-is
                    result.push_str(&rest[start..=(start + 2 + close)]);
                }
            }
            rest = &after_open[close + 1..];
        } else {
            // No closing brace — copy literally
            result.push_str(&rest[start..]);
            rest = "";
        }
    }

    result.push_str(rest);
    result
}

fn resolve_variable(expr: &str, ctx: &VariableContext) -> Option<String> {
    match expr {
        "localWorkspaceFolder" => Some(ctx.local_workspace_folder.clone()),
        "localWorkspaceFolderBasename" => Some(ctx.local_workspace_folder_basename.clone()),
        "containerWorkspaceFolder" => Some(ctx.container_workspace_folder.clone()),
        "containerWorkspaceFolderBasename" => {
            let basename = ctx
                .container_workspace_folder
                .rsplit('/')
                .next()
                .unwrap_or(&ctx.container_workspace_folder);
            Some(basename.to_string())
        }
        _ if expr.starts_with("localEnv:") => {
            let var_part = &expr["localEnv:".len()..];
            // Split on first colon for default value
            if let Some(colon_pos) = var_part.find(':') {
                let var_name = &var_part[..colon_pos];
                let default = &var_part[colon_pos + 1..];
                Some(
                    ctx.env
                        .var(var_name)
                        .unwrap_or_else(|_| default.to_string()),
                )
            } else {
                Some(ctx.env.var(var_part).unwrap_or_default())
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_util::env::{SystemEnv, TestEnv};

    use super::*;

    fn test_ctx() -> VariableContext<'static> {
        // Tests that don't exercise localEnv don't care about the env impl.
        // Use SystemEnv which has no lifetime/allocation concerns.
        VariableContext {
            local_workspace_folder: "/home/user/project".to_string(),
            local_workspace_folder_basename: "project".to_string(),
            container_workspace_folder: "/workspaces/project".to_string(),
            env: &SystemEnv,
        }
    }

    #[test]
    fn no_variables() {
        let ctx = test_ctx();
        assert_eq!(substitute("hello", &ctx), "hello");
    }

    #[test]
    fn local_workspace_folder() {
        let ctx = test_ctx();
        assert_eq!(
            substitute("${localWorkspaceFolder}/src", &ctx),
            "/home/user/project/src"
        );
    }

    #[test]
    fn local_workspace_folder_basename() {
        let ctx = test_ctx();
        assert_eq!(
            substitute("name: ${localWorkspaceFolderBasename}", &ctx),
            "name: project"
        );
    }

    #[test]
    fn container_workspace_folder() {
        let ctx = test_ctx();
        assert_eq!(
            substitute("${containerWorkspaceFolder}/app", &ctx),
            "/workspaces/project/app"
        );
    }

    #[test]
    fn container_workspace_folder_basename() {
        let ctx = test_ctx();
        assert_eq!(
            substitute("${containerWorkspaceFolderBasename}", &ctx),
            "project"
        );
    }

    #[test]
    fn container_workspace_folder_basename_nested() {
        let ctx = VariableContext {
            local_workspace_folder: "/home/user/repos/my-app".to_string(),
            local_workspace_folder_basename: "my-app".to_string(),
            container_workspace_folder: "/workspaces/repos/my-app".to_string(),
            env: &SystemEnv,
        };
        assert_eq!(
            substitute("${containerWorkspaceFolderBasename}", &ctx),
            "my-app"
        );
    }

    #[test]
    fn multiple_variables() {
        let ctx = test_ctx();
        assert_eq!(
            substitute(
                "${localWorkspaceFolder} and ${containerWorkspaceFolder}",
                &ctx
            ),
            "/home/user/project and /workspaces/project"
        );
    }

    #[test]
    fn unknown_variable_left_as_is() {
        let ctx = test_ctx();
        assert_eq!(substitute("${unknownVariable}", &ctx), "${unknownVariable}");
    }

    #[test]
    fn local_env_with_set_variable() {
        let env = TestEnv(HashMap::from([(
            "FABRO_TEST_VAR_SET".into(),
            "hello".into(),
        )]));
        let ctx = VariableContext {
            env: &env,
            ..test_ctx()
        };
        assert_eq!(substitute("${localEnv:FABRO_TEST_VAR_SET}", &ctx), "hello");
    }

    #[test]
    fn local_env_unset_returns_empty() {
        let env = TestEnv(HashMap::new());
        let ctx = VariableContext {
            env: &env,
            ..test_ctx()
        };
        assert_eq!(substitute("${localEnv:FABRO_TEST_VAR_UNSET_123}", &ctx), "");
    }

    #[test]
    fn local_env_with_default_when_unset() {
        let env = TestEnv(HashMap::new());
        let ctx = VariableContext {
            env: &env,
            ..test_ctx()
        };
        assert_eq!(
            substitute("${localEnv:FABRO_TEST_VAR_DEFAULT_456:fallback}", &ctx),
            "fallback"
        );
    }

    #[test]
    fn local_env_with_default_when_set() {
        let env = TestEnv(HashMap::from([(
            "FABRO_TEST_VAR_DEFAULT_SET".into(),
            "actual".into(),
        )]));
        let ctx = VariableContext {
            env: &env,
            ..test_ctx()
        };
        assert_eq!(
            substitute("${localEnv:FABRO_TEST_VAR_DEFAULT_SET:fallback}", &ctx),
            "actual"
        );
    }

    #[test]
    fn no_closing_brace() {
        let ctx = test_ctx();
        assert_eq!(
            substitute("${localWorkspaceFolder", &ctx),
            "${localWorkspaceFolder"
        );
    }

    #[test]
    fn empty_input() {
        let ctx = test_ctx();
        assert_eq!(substitute("", &ctx), "");
    }

    #[test]
    fn dollar_without_brace() {
        let ctx = test_ctx();
        assert_eq!(substitute("$notavar", &ctx), "$notavar");
    }

    #[test]
    fn adjacent_variables() {
        let ctx = test_ctx();
        assert_eq!(
            substitute(
                "${localWorkspaceFolderBasename}${containerWorkspaceFolderBasename}",
                &ctx
            ),
            "projectproject"
        );
    }
}
