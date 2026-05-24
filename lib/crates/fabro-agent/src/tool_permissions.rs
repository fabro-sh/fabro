use fabro_types::{AgentToolCategory, PermissionLevel};

/// Coarse access category for an exposed tool. Returns `None` for unknown
/// names so callers can decide whether to default (legacy CLI permission
/// gate) or surface a distinct "other" category (projection metadata).
pub fn known_tool_category(name: &str) -> Option<AgentToolCategory> {
    match name {
        "read_file" | "read_many_files" | "grep" | "glob" | "list_dir" => {
            Some(AgentToolCategory::Read)
        }
        "write_file" | "edit_file" | "apply_patch" => Some(AgentToolCategory::Write),
        "shell" => Some(AgentToolCategory::Shell),
        "spawn_agent" | "send_input" | "wait" | "close_agent" => Some(AgentToolCategory::Subagent),
        _ => None,
    }
}

/// CLI permission gate category. Unknown tools fall back to `Shell` so they
/// require explicit user approval at any permission level below `Full`.
pub fn tool_category(name: &str) -> AgentToolCategory {
    known_tool_category(name).unwrap_or(AgentToolCategory::Shell)
}

pub fn is_auto_approved(level: PermissionLevel, category: AgentToolCategory) -> bool {
    matches!(
        (level, category),
        (_, AgentToolCategory::Read | AgentToolCategory::Subagent)
            | (
                PermissionLevel::ReadWrite | PermissionLevel::Full,
                AgentToolCategory::Write,
            )
            | (PermissionLevel::Full, AgentToolCategory::Shell)
    )
}

pub fn is_tool_auto_approved(level: PermissionLevel, tool_name: &str) -> bool {
    is_auto_approved(level, tool_category(tool_name))
}
