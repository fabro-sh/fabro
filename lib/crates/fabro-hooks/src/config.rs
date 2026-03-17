use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::types::HookEvent;

/// TLS verification mode for HTTP hooks.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    /// Require `https://` and verify certificates (default).
    #[default]
    Verify,
    /// Require `https://` but skip certificate verification.
    NoVerify,
    /// Allow `http://`; skip certificate verification for `https://`.
    Off,
}

/// How a hook is executed.
#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookType {
    Command {
        command: String,
    },
    Http {
        url: String,
        headers: Option<std::collections::HashMap<String, String>>,
        #[serde(default)]
        allowed_env_vars: Vec<String>,
        #[serde(default)]
        tls: TlsMode,
    },
    Prompt {
        prompt: String,
        model: Option<String>,
    },
    Agent {
        prompt: String,
        model: Option<String>,
        max_tool_rounds: Option<u32>,
    },
}

/// A single hook definition.
#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub struct HookDefinition {
    pub name: Option<String>,
    pub event: HookEvent,
    /// Inline command shorthand — if set, implies `type = "command"`.
    #[serde(default)]
    pub command: Option<String>,
    /// Explicit hook type (command or http). If omitted and `command` is set,
    /// defaults to `Command`.
    #[serde(flatten)]
    pub hook_type: Option<HookType>,
    /// Regex matched against node_id, handler_type, or event-specific fields.
    pub matcher: Option<String>,
    /// Override the event's default blocking behavior.
    pub blocking: Option<bool>,
    /// Timeout in milliseconds (default: 60_000).
    pub timeout_ms: Option<u64>,
    /// Run inside the sandbox (true, default) or on the host (false).
    pub sandbox: Option<bool>,
}

impl HookDefinition {
    /// Resolve the effective hook type: explicit `hook_type` wins, then `command`
    /// shorthand, then error.
    pub fn resolved_hook_type(&self) -> Option<Cow<'_, HookType>> {
        if let Some(ref ht) = self.hook_type {
            return Some(Cow::Borrowed(ht));
        }
        self.command.as_ref().map(|cmd| {
            Cow::Owned(HookType::Command {
                command: cmd.clone(),
            })
        })
    }

    /// Whether this hook is blocking for its event.
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        self.blocking
            .unwrap_or_else(|| self.event.is_blocking_by_default())
    }

    /// Timeout duration for this hook.
    ///
    /// Defaults: 30s for prompt hooks, 60s for all others.
    #[must_use]
    pub fn timeout(&self) -> std::time::Duration {
        if let Some(ms) = self.timeout_ms {
            return std::time::Duration::from_millis(ms);
        }
        let default_ms = match self.resolved_hook_type().as_deref() {
            Some(HookType::Prompt { .. }) => 30_000,
            _ => 60_000,
        };
        std::time::Duration::from_millis(default_ms)
    }

    /// Whether this hook runs in the sandbox.
    #[must_use]
    pub fn runs_in_sandbox(&self) -> bool {
        self.sandbox.unwrap_or(true)
    }

    /// The effective name: explicit name or a generated one.
    #[must_use]
    pub fn effective_name(&self) -> String {
        if let Some(ref n) = self.name {
            return n.clone();
        }
        let event_str = self.event.to_string();
        match self.resolved_hook_type().as_deref() {
            Some(HookType::Command { ref command }) => {
                let short = &command[..fabro_agent::floor_char_boundary(command, 20)];
                format!("{event_str}:{short}")
            }
            Some(HookType::Http { ref url, .. }) => format!("{event_str}:{url}"),
            Some(HookType::Prompt { ref prompt, .. })
            | Some(HookType::Agent { ref prompt, .. }) => {
                let short = &prompt[..fabro_agent::floor_char_boundary(prompt, 20)];
                format!("{event_str}:{short}")
            }
            None => event_str,
        }
    }
}

/// Top-level hook configuration: a list of hook definitions.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct HookConfig {
    #[serde(default)]
    pub hooks: Vec<HookDefinition>,
}

impl HookConfig {
    /// Merge with another config. Concatenates lists; on name collisions, `other` wins.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        let mut by_name: std::collections::HashMap<String, HookDefinition> =
            std::collections::HashMap::new();
        let mut order: Vec<String> = Vec::new();

        for hook in self.hooks {
            let name = hook.effective_name();
            if !by_name.contains_key(&name) {
                order.push(name.clone());
            }
            by_name.insert(name, hook);
        }
        for hook in other.hooks {
            let name = hook.effective_name();
            if !by_name.contains_key(&name) {
                order.push(name.clone());
            }
            by_name.insert(name, hook);
        }

        let hooks = order
            .into_iter()
            .filter_map(|name| by_name.remove(&name))
            .collect();

        Self { hooks }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_command_shorthand() {
        let toml = r#"
[[hooks]]
event = "stage_start"
command = "./scripts/pre-check.sh"
"#;
        let config: HookConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.hooks.len(), 1);
        let hook = &config.hooks[0];
        assert_eq!(hook.event, HookEvent::StageStart);
        assert_eq!(hook.command.as_deref(), Some("./scripts/pre-check.sh"));
        let resolved = hook.resolved_hook_type().unwrap();
        assert!(
            matches!(&*resolved, HookType::Command { command } if command == "./scripts/pre-check.sh")
        );
    }

    #[test]
    fn parse_explicit_command_type() {
        let toml = r#"
[[hooks]]
event = "run_start"
type = "command"
command = "echo hello"
"#;
        let config: HookConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.hooks.len(), 1);
        let hook = &config.hooks[0];
        assert_eq!(hook.event, HookEvent::RunStart);
        assert!(hook.resolved_hook_type().is_some());
    }

    #[test]
    fn parse_http_hook() {
        let toml = r#"
[[hooks]]
event = "run_complete"
type = "http"
url = "https://hooks.example.com/done"
"#;
        let config: HookConfig = toml::from_str(toml).unwrap();
        let hook = &config.hooks[0];
        assert!(matches!(
            hook.resolved_hook_type().as_deref(),
            Some(HookType::Http { url, .. }) if url == "https://hooks.example.com/done"
        ));
    }

    #[test]
    fn parse_http_hook_with_allowed_env_vars() {
        let toml = r#"
[[hooks]]
event = "run_start"
type = "http"
url = "https://hooks.example.com/start"
allowed_env_vars = ["API_KEY", "SECRET"]

[hooks.headers]
Authorization = "Bearer $API_KEY"
"#;
        let config: HookConfig = toml::from_str(toml).unwrap();
        let hook = &config.hooks[0];
        match &*hook.resolved_hook_type().unwrap() {
            HookType::Http {
                url,
                headers,
                allowed_env_vars,
                ..
            } => {
                assert_eq!(url, "https://hooks.example.com/start");
                assert_eq!(allowed_env_vars, &["API_KEY", "SECRET"]);
                assert_eq!(
                    headers.as_ref().unwrap().get("Authorization").unwrap(),
                    "Bearer $API_KEY"
                );
            }
            _ => panic!("expected Http hook type"),
        }
    }

    #[test]
    fn parse_http_hook_allowed_env_vars_defaults_empty() {
        let toml = r#"
[[hooks]]
event = "run_complete"
type = "http"
url = "https://hooks.example.com/done"
"#;
        let config: HookConfig = toml::from_str(toml).unwrap();
        let hook = &config.hooks[0];
        match &*hook.resolved_hook_type().unwrap() {
            HookType::Http {
                allowed_env_vars, ..
            } => {
                assert!(allowed_env_vars.is_empty());
            }
            _ => panic!("expected Http hook type"),
        }
    }

    #[test]
    fn parse_full_hook_definition() {
        let toml = r#"
[[hooks]]
name = "pre-check"
event = "stage_start"
command = "./check.sh"
matcher = "agent_loop"
blocking = true
timeout_ms = 30000
sandbox = false
"#;
        let config: HookConfig = toml::from_str(toml).unwrap();
        let hook = &config.hooks[0];
        assert_eq!(hook.name.as_deref(), Some("pre-check"));
        assert_eq!(hook.event, HookEvent::StageStart);
        assert_eq!(hook.matcher.as_deref(), Some("agent_loop"));
        assert!(hook.is_blocking());
        assert_eq!(hook.timeout(), std::time::Duration::from_millis(30_000));
        assert!(!hook.runs_in_sandbox());
    }

    #[test]
    fn blocking_defaults_to_event() {
        let blocking_def = HookDefinition {
            name: None,
            event: HookEvent::StageStart,
            command: Some("echo".into()),
            hook_type: None,
            matcher: None,
            blocking: None,
            timeout_ms: None,
            sandbox: None,
        };
        assert!(blocking_def.is_blocking());

        let non_blocking_def = HookDefinition {
            event: HookEvent::StageComplete,
            ..blocking_def.clone()
        };
        assert!(!non_blocking_def.is_blocking());
    }

    #[test]
    fn blocking_override() {
        let def = HookDefinition {
            name: None,
            event: HookEvent::StageComplete,
            command: Some("echo".into()),
            hook_type: None,
            matcher: None,
            blocking: Some(true),
            timeout_ms: None,
            sandbox: None,
        };
        assert!(def.is_blocking());
    }

    #[test]
    fn timeout_defaults_to_60s() {
        let def = HookDefinition {
            name: None,
            event: HookEvent::RunStart,
            command: Some("echo".into()),
            hook_type: None,
            matcher: None,
            blocking: None,
            timeout_ms: None,
            sandbox: None,
        };
        assert_eq!(def.timeout(), std::time::Duration::from_secs(60));
    }

    #[test]
    fn sandbox_defaults_to_true() {
        let def = HookDefinition {
            name: None,
            event: HookEvent::RunStart,
            command: Some("echo".into()),
            hook_type: None,
            matcher: None,
            blocking: None,
            timeout_ms: None,
            sandbox: None,
        };
        assert!(def.runs_in_sandbox());
    }

    #[test]
    fn effective_name_uses_explicit() {
        let def = HookDefinition {
            name: Some("my-hook".into()),
            event: HookEvent::RunStart,
            command: Some("echo hi".into()),
            hook_type: None,
            matcher: None,
            blocking: None,
            timeout_ms: None,
            sandbox: None,
        };
        assert_eq!(def.effective_name(), "my-hook");
    }

    #[test]
    fn effective_name_generated_from_event_and_command() {
        let def = HookDefinition {
            name: None,
            event: HookEvent::RunStart,
            command: Some("echo hi".into()),
            hook_type: None,
            matcher: None,
            blocking: None,
            timeout_ms: None,
            sandbox: None,
        };
        assert_eq!(def.effective_name(), "run_start:echo hi");
    }

    #[test]
    fn config_merge_concatenates() {
        let a = HookConfig {
            hooks: vec![HookDefinition {
                name: Some("hook-a".into()),
                event: HookEvent::RunStart,
                command: Some("echo a".into()),
                hook_type: None,
                matcher: None,
                blocking: None,
                timeout_ms: None,
                sandbox: None,
            }],
        };
        let b = HookConfig {
            hooks: vec![HookDefinition {
                name: Some("hook-b".into()),
                event: HookEvent::RunComplete,
                command: Some("echo b".into()),
                hook_type: None,
                matcher: None,
                blocking: None,
                timeout_ms: None,
                sandbox: None,
            }],
        };
        let merged = a.merge(b);
        assert_eq!(merged.hooks.len(), 2);
        assert_eq!(merged.hooks[0].name.as_deref(), Some("hook-a"));
        assert_eq!(merged.hooks[1].name.as_deref(), Some("hook-b"));
    }

    #[test]
    fn config_merge_name_collision_later_wins() {
        let a = HookConfig {
            hooks: vec![HookDefinition {
                name: Some("shared".into()),
                event: HookEvent::RunStart,
                command: Some("echo a".into()),
                hook_type: None,
                matcher: None,
                blocking: None,
                timeout_ms: None,
                sandbox: None,
            }],
        };
        let b = HookConfig {
            hooks: vec![HookDefinition {
                name: Some("shared".into()),
                event: HookEvent::RunComplete,
                command: Some("echo b".into()),
                hook_type: None,
                matcher: None,
                blocking: None,
                timeout_ms: None,
                sandbox: None,
            }],
        };
        let merged = a.merge(b);
        assert_eq!(merged.hooks.len(), 1);
        assert_eq!(merged.hooks[0].event, HookEvent::RunComplete);
    }

    #[test]
    fn parse_http_hook_tls_defaults_to_verify() {
        let toml = r#"
[[hooks]]
event = "run_complete"
type = "http"
url = "https://hooks.example.com/done"
"#;
        let config: HookConfig = toml::from_str(toml).unwrap();
        let hook = &config.hooks[0];
        match &*hook.resolved_hook_type().unwrap() {
            HookType::Http { tls, .. } => assert_eq!(*tls, TlsMode::Verify),
            _ => panic!("expected Http hook type"),
        }
    }

    #[test]
    fn parse_http_hook_tls_no_verify() {
        let toml = r#"
[[hooks]]
event = "run_complete"
type = "http"
url = "https://hooks.example.com/done"
tls = "no_verify"
"#;
        let config: HookConfig = toml::from_str(toml).unwrap();
        let hook = &config.hooks[0];
        match &*hook.resolved_hook_type().unwrap() {
            HookType::Http { tls, .. } => assert_eq!(*tls, TlsMode::NoVerify),
            _ => panic!("expected Http hook type"),
        }
    }

    #[test]
    fn parse_http_hook_tls_off() {
        let toml = r#"
[[hooks]]
event = "run_complete"
type = "http"
url = "http://localhost:8080/done"
tls = "off"
"#;
        let config: HookConfig = toml::from_str(toml).unwrap();
        let hook = &config.hooks[0];
        match &*hook.resolved_hook_type().unwrap() {
            HookType::Http { tls, .. } => assert_eq!(*tls, TlsMode::Off),
            _ => panic!("expected Http hook type"),
        }
    }

    #[test]
    fn parse_prompt_hook() {
        let toml = r#"
[[hooks]]
event = "stage_start"
type = "prompt"
prompt = "Should this stage proceed?"
model = "haiku"
"#;
        let config: HookConfig = toml::from_str(toml).unwrap();
        let hook = &config.hooks[0];
        assert!(matches!(
            hook.resolved_hook_type().as_deref(),
            Some(HookType::Prompt { prompt, model })
                if prompt == "Should this stage proceed?" && *model == Some("haiku".into())
        ));
    }

    #[test]
    fn parse_agent_hook() {
        let toml = r#"
[[hooks]]
event = "run_complete"
type = "agent"
prompt = "Verify tests pass."
model = "sonnet"
max_tool_rounds = 10
"#;
        let config: HookConfig = toml::from_str(toml).unwrap();
        let hook = &config.hooks[0];
        assert!(matches!(
            hook.resolved_hook_type().as_deref(),
            Some(HookType::Agent { prompt, model, max_tool_rounds })
                if prompt == "Verify tests pass."
                && *model == Some("sonnet".into())
                && *max_tool_rounds == Some(10)
        ));
    }

    #[test]
    fn prompt_hook_default_timeout_30s() {
        let def = HookDefinition {
            name: None,
            event: HookEvent::RunStart,
            command: None,
            hook_type: Some(HookType::Prompt {
                prompt: "check".into(),
                model: None,
            }),
            matcher: None,
            blocking: None,
            timeout_ms: None,
            sandbox: None,
        };
        assert_eq!(def.timeout(), std::time::Duration::from_secs(30));
    }

    #[test]
    fn agent_hook_default_timeout_60s() {
        let def = HookDefinition {
            name: None,
            event: HookEvent::RunStart,
            command: None,
            hook_type: Some(HookType::Agent {
                prompt: "check".into(),
                model: None,
                max_tool_rounds: None,
            }),
            matcher: None,
            blocking: None,
            timeout_ms: None,
            sandbox: None,
        };
        assert_eq!(def.timeout(), std::time::Duration::from_secs(60));
    }

    #[test]
    fn effective_name_generated_from_prompt_hook() {
        let def = HookDefinition {
            name: None,
            event: HookEvent::StageStart,
            command: None,
            hook_type: Some(HookType::Prompt {
                prompt: "Should this stage proceed?".into(),
                model: None,
            }),
            matcher: None,
            blocking: None,
            timeout_ms: None,
            sandbox: None,
        };
        assert!(def.effective_name().starts_with("stage_start:"));
    }

    #[test]
    fn parse_multiple_hooks() {
        let toml = r#"
[[hooks]]
event = "run_start"
command = "echo start"

[[hooks]]
event = "stage_complete"
command = "echo done"
matcher = "agent_loop"
"#;
        let config: HookConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.hooks.len(), 2);
        assert_eq!(config.hooks[0].event, HookEvent::RunStart);
        assert_eq!(config.hooks[1].event, HookEvent::StageComplete);
        assert_eq!(config.hooks[1].matcher.as_deref(), Some("agent_loop"));
    }
}
