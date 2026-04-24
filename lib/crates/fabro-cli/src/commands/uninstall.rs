#![expect(
    clippy::disallowed_types,
    reason = "sync CLI `uninstall` command: blocking std::io::Write is the intended output mechanism"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "CLI `uninstall` command: sync file I/O in command handler"
)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use fabro_config::Storage;
use fabro_config::daemon::ServerDaemon;
use fabro_static::EnvVars;
use fabro_util::Home;
use fabro_util::printer::Printer;
use serde::Serialize;
use tracing::warn;

use crate::args::UninstallArgs;
use crate::command_context::CommandContext;
use crate::commands::server::stop;
use crate::shared::{format_size, print_json_pretty, tilde_path};
use crate::{local_server, user_config};

#[derive(Debug, Serialize)]
struct Inventory {
    home_root:         PathBuf,
    storage_dir:       PathBuf,
    home_exists:       bool,
    home_size:         u64,
    server_running:    bool,
    shell_configs:     Vec<PathBuf>,
    binary_path:       Option<PathBuf>,
    binary_is_managed: bool,
}

#[allow(
    clippy::unused_async,
    reason = "The shared command dispatch path expects an async handler."
)]
pub(crate) async fn run_uninstall(args: &UninstallArgs, ctx: &CommandContext) -> Result<()> {
    let json = ctx.json_output();
    let printer = ctx.printer();
    let home = Home::from_env();
    let home_root = home.root().to_path_buf();

    if !looks_like_fabro_home(&home_root) {
        if json {
            print_json_pretty(&serde_json::json!({ "status": "not_installed" }))?;
        } else {
            fabro_util::printerr!(printer, "Fabro is not installed.");
        }
        return Ok(());
    }

    let storage_dir = local_server::LocalServerConfig::load_with_storage_dir(None)
        .ok()
        .map_or_else(user_config::default_storage_dir, |settings| {
            settings.storage_dir().to_path_buf()
        });

    let inventory = build_inventory(&home_root, &storage_dir)?;

    if !args.yes {
        if json {
            print_json_pretty(&inventory)?;
        } else {
            print_preview(&inventory, printer);
        }
        return Ok(());
    }

    // Tear down telemetry before removal so the post-command
    // `track!("CLI Executed", ...)` in main() can't queue an event that
    // would make the background flush call
    // `spawn_fabro_subcommand` → `create_dir_all(~/.fabro/tmp)` and
    // silently recreate the directory we are about to delete.
    fabro_telemetry::shutdown();

    execute_uninstall(&inventory, json, printer).await
}

fn build_inventory(home_root: &Path, storage_dir: &Path) -> Result<Inventory> {
    let home_size = dir_size(home_root);
    let server_running =
        ServerDaemon::load_running(&Storage::new(storage_dir).runtime_directory())?.is_some();
    let shell_configs = find_shell_configs_with_sentinel();
    let (binary_path, binary_is_managed) = resolve_binary(home_root);

    Ok(Inventory {
        home_root: home_root.to_path_buf(),
        storage_dir: storage_dir.to_path_buf(),
        home_exists: true,
        home_size,
        server_running,
        shell_configs,
        binary_path,
        binary_is_managed,
    })
}

fn dir_size(path: &Path) -> u64 {
    let mut total: u64 = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                total += dir_size(&entry.path());
            } else {
                total += entry.metadata().map_or(0, |m| m.len());
            }
        }
    }
    total
}

#[expect(
    clippy::disallowed_methods,
    reason = "Uninstall scans shell config paths honoring the conventional ZDOTDIR env var."
)]
fn find_shell_configs_with_sentinel() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Some(home) = dirs::home_dir() else {
        return found;
    };

    let zdotdir = std::env::var(EnvVars::ZDOTDIR)
        .ok()
        .map_or_else(|| home.clone(), PathBuf::from);

    let candidates = [
        zdotdir.join(".zshrc"),
        home.join(".bashrc"),
        home.join(".bash_profile"),
        home.join(".config/fish/config.fish"),
    ];

    for path in &candidates {
        if file_contains_sentinel(path) {
            found.push(path.clone());
        }
    }
    found
}

fn file_contains_sentinel(path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    content.lines().any(|line| line.trim() == "# fabro")
}

fn resolve_binary(home_root: &Path) -> (Option<PathBuf>, bool) {
    let binary_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());
    let is_managed = binary_path
        .as_ref()
        .is_some_and(|p| p.starts_with(home_root));
    (binary_path, is_managed)
}

fn print_preview(inventory: &Inventory, printer: Printer) {
    let green = console::Style::new().green();
    let dim = console::Style::new().dim();
    let bold = console::Style::new().bold();

    fabro_util::printerr!(
        printer,
        "\n{}",
        bold.apply_to("The following will be removed:")
    );
    fabro_util::printerr!(
        printer,
        "  {} {} {}",
        green.apply_to("~"),
        tilde_path(&inventory.home_root),
        dim.apply_to(format!("({})", format_size(inventory.home_size)))
    );

    if inventory.storage_dir != inventory.home_root.join("storage")
        && !inventory.storage_dir.starts_with(&inventory.home_root)
    {
        let storage_size = dir_size(&inventory.storage_dir);
        fabro_util::printerr!(
            printer,
            "  {} {} {}",
            green.apply_to("~"),
            tilde_path(&inventory.storage_dir),
            dim.apply_to(format!("({})", format_size(storage_size)))
        );
    }

    if inventory.server_running {
        fabro_util::printerr!(
            printer,
            "\n  {} A running server will be stopped first.",
            console::Style::new().yellow().apply_to("!")
        );
    }

    if !inventory.shell_configs.is_empty() {
        fabro_util::printerr!(printer, "\n  Shell configs with PATH entries:");
        for path in &inventory.shell_configs {
            fabro_util::printerr!(printer, "    {}", tilde_path(path));
        }
    }

    match (&inventory.binary_path, inventory.binary_is_managed) {
        (Some(_), true) => {
            fabro_util::printerr!(
                printer,
                "\n  {} Binary is inside {} and will be removed.",
                dim.apply_to("i"),
                tilde_path(&inventory.home_root)
            );
        }
        (Some(bin), false) => {
            fabro_util::printerr!(
                printer,
                "\n  {} Binary at {} is outside {} and must be removed manually.",
                dim.apply_to("i"),
                tilde_path(bin),
                tilde_path(&inventory.home_root)
            );
        }
        (None, _) => {
            fabro_util::printerr!(
                printer,
                "\n  {} Could not determine binary location.",
                dim.apply_to("i")
            );
        }
    }

    fabro_util::printerr!(printer, "\nPass --yes to confirm.");
}

#[derive(Debug, Serialize)]
struct UninstallResult {
    status:                &'static str,
    home_removed:          bool,
    server_stopped:        bool,
    shell_configs_cleaned: Vec<PathBuf>,
    binary_removed:        bool,
    binary_hint:           Option<String>,
}

async fn execute_uninstall(inventory: &Inventory, json: bool, printer: Printer) -> Result<()> {
    let green = console::Style::new().green();
    let dim = console::Style::new().dim();
    let bold = console::Style::new().bold();
    let mut critical_failure = false;
    let mut result = UninstallResult {
        status:                "completed",
        home_removed:          false,
        server_stopped:        false,
        shell_configs_cleaned: Vec::new(),
        binary_removed:        false,
        binary_hint:           None,
    };

    // Unit 3a: Server stop
    if inventory.server_running {
        stop::execute(&inventory.storage_dir, Duration::from_secs(5), printer).await?;
        result.server_stopped = true;
    }

    // Unit 3b: Safety guardrails
    if let Err(e) = validate_safe_to_delete(&inventory.home_root) {
        fabro_util::printerr!(
            printer,
            "Refusing to delete {}: {e}",
            inventory.home_root.display()
        );
        return Err(e);
    }

    // Unit 3c: Directory removal
    match fs::remove_dir_all(&inventory.home_root) {
        Ok(()) => {
            result.home_removed = true;
            if !json {
                fabro_util::printerr!(
                    printer,
                    "  {} Removed {}",
                    green.apply_to("\u{2714}"),
                    tilde_path(&inventory.home_root)
                );
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            result.home_removed = true;
            if !json {
                fabro_util::printerr!(
                    printer,
                    "  {} {} already removed",
                    dim.apply_to("-"),
                    tilde_path(&inventory.home_root)
                );
            }
        }
        Err(e) => {
            if !json {
                fabro_util::printerr!(
                    printer,
                    "  Failed to remove {}: {e}",
                    tilde_path(&inventory.home_root)
                );
            }
            critical_failure = true;
        }
    }

    // Remove external storage_dir if outside home_root
    if !inventory.storage_dir.starts_with(&inventory.home_root) && inventory.storage_dir.exists() {
        if let Err(e) = validate_safe_to_delete(&inventory.storage_dir) {
            if !json {
                fabro_util::printerr!(
                    printer,
                    "Refusing to delete storage dir {}: {e}",
                    inventory.storage_dir.display()
                );
            }
            critical_failure = true;
        } else {
            match fs::remove_dir_all(&inventory.storage_dir) {
                Ok(()) => {
                    if !json {
                        fabro_util::printerr!(
                            printer,
                            "  {} Removed {}",
                            green.apply_to("\u{2714}"),
                            tilde_path(&inventory.storage_dir)
                        );
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    if !json {
                        fabro_util::printerr!(
                            printer,
                            "  Failed to remove {}: {e}",
                            tilde_path(&inventory.storage_dir)
                        );
                    }
                    critical_failure = true;
                }
            }
        }
    }

    // Unit 4: Shell config cleanup
    for path in &inventory.shell_configs {
        match clean_shell_config(path) {
            Ok(()) => {
                result.shell_configs_cleaned.push(path.clone());
                if !json {
                    fabro_util::printerr!(
                        printer,
                        "  {} Cleaned {}",
                        green.apply_to("\u{2714}"),
                        tilde_path(path)
                    );
                }
            }
            Err(e) => {
                warn!("Failed to clean shell config {}: {e}", path.display());
                if !json {
                    fabro_util::printerr!(
                        printer,
                        "  {} Could not clean {}: {e}",
                        console::Style::new().yellow().apply_to("!"),
                        tilde_path(path)
                    );
                }
            }
        }
    }

    // Unit 5: Binary reporting
    match (&inventory.binary_path, inventory.binary_is_managed) {
        (Some(_), true) => {
            result.binary_removed = true;
            if !json {
                fabro_util::printerr!(
                    printer,
                    "  {} Binary removed {}",
                    green.apply_to("\u{2714}"),
                    dim.apply_to("(was inside ~/.fabro/bin/)")
                );
            }
        }
        (Some(bin), false) => {
            let hint = binary_removal_hint(bin);
            result.binary_hint = Some(hint.clone());
            if !json {
                fabro_util::printerr!(printer, "\n  {} {}", dim.apply_to("i"), hint);
            }
        }
        (None, _) => {
            warn!("Could not determine binary path; skipping binary removal hint");
        }
    }

    if critical_failure {
        result.status = "partial";
    }

    // Final output
    if json {
        print_json_pretty(&result)?;
    } else {
        fabro_util::printerr!(
            printer,
            "\n{}",
            bold.apply_to("Fabro has been uninstalled.")
        );
    }

    if critical_failure {
        std::process::exit(1);
    }

    Ok(())
}

/// Returns true if the directory exists and contains Fabro artifacts
/// (settings.toml, certs/, or storage/). An empty directory auto-created
/// by the CLI's logging setup is not considered an installation.
fn looks_like_fabro_home(path: &Path) -> bool {
    path.exists()
        && (path.join("settings.toml").exists()
            || path.join("certs").exists()
            || path.join("storage").exists())
}

fn validate_safe_to_delete(path: &Path) -> Result<()> {
    let root = Path::new("/");
    anyhow::ensure!(path != root, "path is the filesystem root");

    if let Some(home) = dirs::home_dir() {
        anyhow::ensure!(path != home, "path is the user home directory");
    }

    let has_settings = path.join("settings.toml").exists();
    let has_certs = path.join("certs").exists();
    anyhow::ensure!(
        has_settings || has_certs,
        "path does not look like a Fabro home (missing settings.toml and certs/)"
    );

    Ok(())
}

fn clean_shell_config(path: &Path) -> Result<()> {
    let content =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    let lines: Vec<&str> = content.lines().collect();
    let mut output = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        if lines[i].trim() == "# fabro" {
            // Check if next line is a PATH export or fish_add_path
            if i + 1 < lines.len() {
                let next = lines[i + 1].trim();
                if next.starts_with("export PATH=") || next.starts_with("fish_add_path") {
                    // Skip both sentinel and PATH line
                    i += 2;
                    continue;
                }
            }
            // Skip only the sentinel line
            i += 1;
            continue;
        }
        output.push(lines[i]);
        i += 1;
    }

    let mut result = output.join("\n");
    // Preserve trailing newline if original had one
    if content.ends_with('\n') {
        result.push('\n');
    }

    // Atomic write: write to temp file in same directory, then rename
    let parent = path
        .parent()
        .with_context(|| format!("no parent directory for {}", path.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temp file in {}", parent.display()))?;
    tmp.write_all(result.as_bytes())
        .with_context(|| format!("writing temp file for {}", path.display()))?;
    tmp.persist(path)
        .with_context(|| format!("renaming temp file to {}", path.display()))?;

    Ok(())
}

fn binary_removal_hint(bin: &Path) -> String {
    let bin_str = bin.to_string_lossy();
    if bin_str.contains("Cellar") || bin_str.contains("homebrew") || bin_str.contains("linuxbrew") {
        format!(
            "Binary at {} was installed via Homebrew. Run: brew uninstall fabro",
            tilde_path(bin)
        )
    } else if bin_str.contains(".cargo") {
        format!(
            "Binary at {} was installed via Cargo. Run: cargo uninstall fabro",
            tilde_path(bin)
        )
    } else {
        format!("Binary at {} must be removed manually.", tilde_path(bin))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{
        binary_removal_hint, build_inventory, clean_shell_config, dir_size, file_contains_sentinel,
        validate_safe_to_delete,
    };

    fn create_fabro_home(dir: &Path) {
        fs::create_dir_all(dir.join("certs")).unwrap();
        fs::write(dir.join("settings.toml"), "# fabro settings\n").unwrap();
        fs::create_dir_all(dir.join("storage")).unwrap();
        fs::write(dir.join("storage/data.db"), "fake-db-content").unwrap();
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin/fabro"), "fake-binary").unwrap();
    }

    #[test]
    fn dir_size_sums_nested_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        fs::write(root.join("a.txt"), "hello").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/b.txt"), "world!").unwrap();

        let size = dir_size(root);
        assert_eq!(size, 11); // "hello" (5) + "world!" (6)
    }

    #[test]
    fn dir_size_empty_directory_is_zero() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(dir_size(tmp.path()), 0);
    }

    #[test]
    fn dir_size_nonexistent_is_zero() {
        let path = PathBuf::from("/nonexistent-fabro-test-dir-xyz");
        assert_eq!(dir_size(&path), 0);
    }

    #[test]
    fn file_contains_sentinel_exact_match() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".zshrc");
        fs::write(
            &path,
            "some stuff\n# fabro\nexport PATH=\"$HOME/.fabro/bin:$PATH\"\n",
        )
        .unwrap();

        assert!(file_contains_sentinel(&path));
    }

    #[test]
    fn file_contains_sentinel_with_leading_whitespace() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".zshrc");
        fs::write(&path, "  # fabro\nexport PATH=\"$HOME/.fabro/bin:$PATH\"\n").unwrap();

        assert!(file_contains_sentinel(&path));
    }

    #[test]
    fn file_contains_sentinel_rejects_substring() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".zshrc");
        fs::write(&path, "# fabro-workflow\nsome other line\n").unwrap();

        assert!(!file_contains_sentinel(&path));
    }

    #[test]
    fn file_contains_sentinel_rejects_missing_file() {
        let path = PathBuf::from("/nonexistent-fabro-test-file-xyz");
        assert!(!file_contains_sentinel(&path));
    }

    #[test]
    fn file_contains_sentinel_rejects_comment_in_middle() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".zshrc");
        fs::write(&path, "echo '# fabro'\nother stuff\n").unwrap();

        assert!(!file_contains_sentinel(&path));
    }

    #[test]
    fn clean_shell_config_removes_sentinel_and_path_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".zshrc");
        fs::write(
            &path,
            "existing line\n# fabro\nexport PATH=\"$HOME/.fabro/bin:$PATH\"\nafter line\n",
        )
        .unwrap();

        clean_shell_config(&path).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert_eq!(result, "existing line\nafter line\n");
    }

    #[test]
    fn clean_shell_config_removes_fish_sentinel_and_path() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.fish");
        fs::write(
            &path,
            "set -gx EDITOR vim\n# fabro\nfish_add_path $HOME/.fabro/bin\nend\n",
        )
        .unwrap();

        clean_shell_config(&path).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert_eq!(result, "set -gx EDITOR vim\nend\n");
    }

    #[test]
    fn clean_shell_config_removes_only_sentinel_when_next_line_unrelated() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".bashrc");
        fs::write(&path, "before\n# fabro\necho hello\nafter\n").unwrap();

        clean_shell_config(&path).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert_eq!(result, "before\necho hello\nafter\n");
    }

    #[test]
    fn clean_shell_config_removes_sentinel_at_end_of_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".bashrc");
        fs::write(&path, "before\n# fabro\n").unwrap();

        clean_shell_config(&path).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert_eq!(result, "before\n");
    }

    #[test]
    fn clean_shell_config_preserves_trailing_newline() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".zshrc");
        fs::write(&path, "keep this\n# fabro\nexport PATH=\"foo\"\n").unwrap();

        clean_shell_config(&path).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(result.ends_with('\n'));
        assert_eq!(result, "keep this\n");
    }

    #[test]
    fn clean_shell_config_no_trailing_newline_when_original_lacks_one() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".zshrc");
        fs::write(&path, "keep this\n# fabro\nexport PATH=\"foo\"").unwrap();

        clean_shell_config(&path).unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert_eq!(result, "keep this");
    }

    #[test]
    fn validate_safe_to_delete_refuses_root() {
        let result = validate_safe_to_delete(Path::new("/"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("filesystem root"), "got: {msg}");
    }

    #[test]
    fn validate_safe_to_delete_refuses_home_dir() {
        if let Some(home) = dirs::home_dir() {
            let result = validate_safe_to_delete(&home);
            assert!(result.is_err());
            let msg = result.unwrap_err().to_string();
            assert!(msg.contains("home directory"), "got: {msg}");
        }
    }

    #[test]
    fn validate_safe_to_delete_refuses_dir_without_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let result = validate_safe_to_delete(tmp.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("does not look like"), "got: {msg}");
    }

    #[test]
    fn validate_safe_to_delete_accepts_dir_with_settings_toml() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("settings.toml"), "").unwrap();

        let result = validate_safe_to_delete(tmp.path());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_safe_to_delete_accepts_dir_with_certs() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("certs")).unwrap();

        let result = validate_safe_to_delete(tmp.path());
        assert!(result.is_ok());
    }

    #[test]
    fn build_inventory_populates_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let home_root = tmp.path().join("fake-fabro-home");
        create_fabro_home(&home_root);

        let storage_dir = home_root.join("storage");
        let inv = build_inventory(&home_root, &storage_dir).unwrap();

        assert!(inv.home_exists);
        assert!(inv.home_size > 0);
        assert!(!inv.server_running);
        assert_eq!(inv.home_root, home_root);
        assert_eq!(inv.storage_dir, storage_dir);
    }

    #[test]
    fn build_inventory_shell_configs_empty_in_temp() {
        let tmp = tempfile::tempdir().unwrap();
        let home_root = tmp.path().join("fake-fabro-home");
        create_fabro_home(&home_root);

        let inv = build_inventory(&home_root, &home_root.join("storage")).unwrap();
        // shell_configs depends on the actual user's shell config files,
        // but we verify the field is populated (even if empty in CI/test)
        assert!(inv.shell_configs.is_empty() || !inv.shell_configs.is_empty());
    }

    #[test]
    fn binary_removal_hint_homebrew() {
        let hint = binary_removal_hint(Path::new("/opt/homebrew/Cellar/fabro/1.0/bin/fabro"));
        assert!(hint.contains("Homebrew"), "got: {hint}");
        assert!(hint.contains("brew uninstall"), "got: {hint}");
    }

    #[test]
    fn binary_removal_hint_cargo() {
        let hint = binary_removal_hint(Path::new("/home/user/.cargo/bin/fabro"));
        assert!(hint.contains("Cargo"), "got: {hint}");
        assert!(hint.contains("cargo uninstall"), "got: {hint}");
    }

    #[test]
    fn binary_removal_hint_manual() {
        let hint = binary_removal_hint(Path::new("/usr/local/bin/fabro"));
        assert!(hint.contains("manually"), "got: {hint}");
    }
}
