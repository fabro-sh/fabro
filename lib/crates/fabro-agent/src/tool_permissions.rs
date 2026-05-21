use fabro_types::PermissionLevel;

pub fn tool_category(name: &str) -> &'static str {
    match name {
        "read_file" | "read_many_files" | "grep" | "glob" | "list_dir" => "read",
        "write_file" | "edit_file" | "apply_patch" => "write",
        "spawn_agent" | "send_input" | "wait" | "close_agent" => "subagent",
        _ => "shell",
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
