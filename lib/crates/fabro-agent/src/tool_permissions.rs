use fabro_types::PermissionLevel;

pub fn tool_category(name: &str) -> &'static str {
    known_tool_category(name).unwrap_or("shell")
}

pub fn known_tool_category(name: &str) -> Option<&'static str> {
    match name {
        "read_file" | "read_many_files" | "grep" | "glob" | "list_dir" => Some("read"),
        "write_file" | "edit_file" | "apply_patch" => Some("write"),
        "shell" => Some("shell"),
        "spawn_agent" | "send_input" | "wait" | "close_agent" => Some("subagent"),
        _ => None,
    }
}

pub fn is_auto_approved(level: PermissionLevel, category: &str) -> bool {
    matches!(
        (level, category),
        (_, "read" | "subagent")
            | (PermissionLevel::ReadWrite | PermissionLevel::Full, "write")
            | (PermissionLevel::Full, "shell")
    )
}

pub fn is_tool_auto_approved(level: PermissionLevel, tool_name: &str) -> bool {
    is_auto_approved(level, tool_category(tool_name))
}
