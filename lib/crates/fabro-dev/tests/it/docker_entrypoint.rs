use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;

use super::{output_text, read_file, workspace_root};

#[test]
fn entrypoint_adds_fabro_to_existing_docker_socket_group() {
    let fixture = EntryPointFixture::new();
    fixture.write_root_id_command("1000");
    fixture.write_socket_gid_command(0);
    fixture.write_fake_command("awk", "echo root\n");
    fixture.write_default_logging_commands();

    let _socket = UnixListener::bind(fixture.socket_path()).expect("binding fake Docker socket");
    fixture.run_entrypoint();

    let log = fixture.log();
    assert!(
        log.contains("addgroup fabro root\n"),
        "entrypoint should add fabro to the socket's existing group:\n{log}"
    );
    assert!(
        log.contains("su-exec fabro true\n"),
        "entrypoint should still drop privileges through su-exec:\n{log}"
    );
}

#[test]
fn entrypoint_creates_group_when_docker_socket_gid_is_unknown() {
    let fixture = EntryPointFixture::new();
    fixture.write_root_id_command("1000");
    fixture.write_socket_gid_command(1234);
    fixture.write_fake_command("awk", "true\n");
    fixture.write_default_logging_commands();

    let _socket = UnixListener::bind(fixture.socket_path()).expect("binding fake Docker socket");
    fixture.run_entrypoint();

    let log = fixture.log();
    assert!(
        log.contains("addgroup -S -g 1234 docker-sock-1234\n"),
        "entrypoint should create a group for an unknown socket gid:\n{log}"
    );
    assert!(
        log.contains("addgroup fabro docker-sock-1234\n"),
        "entrypoint should add fabro to the generated socket group:\n{log}"
    );
}

struct EntryPointFixture {
    root: tempfile::TempDir,
}

impl EntryPointFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("creating entrypoint test dir");
        std::fs::create_dir(root.path().join("bin")).expect("creating fake bin dir");
        Self { root }
    }

    fn socket_path(&self) -> std::path::PathBuf {
        self.root.path().join("docker.sock")
    }

    fn log_path(&self) -> std::path::PathBuf {
        self.root.path().join("calls.log")
    }

    fn bin_path(&self) -> std::path::PathBuf {
        self.root.path().join("bin")
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "integration test runs the POSIX entrypoint script as a subprocess"
    )]
    fn run_entrypoint(&self) {
        let output = Command::new("/bin/sh")
            .arg(workspace_root().join("docker/entrypoint.sh"))
            .arg("true")
            .env(
                "DOCKER_HOST",
                format!("unix://{}", self.socket_path().display()),
            )
            .env("FABRO_HOME", self.root.path().join("home"))
            .env("PATH", self.bin_path())
            .env("ENTRYPOINT_TEST_LOG", self.log_path())
            .output()
            .expect("running entrypoint");

        assert!(
            output.status.success(),
            "entrypoint failed\nstdout:\n{}\nstderr:\n{}",
            output_text(&output.stdout),
            output_text(&output.stderr)
        );
    }

    fn log(&self) -> String {
        read_file(self.root.path(), "calls.log")
    }

    fn write_logging_command(&self, name: &str) {
        self.write_fake_command(
            name,
            r#"printf '%s %s\n' "${0##*/}" "$*" >> "$ENTRYPOINT_TEST_LOG"
"#,
        );
    }

    fn write_default_logging_commands(&self) {
        self.write_logging_command("addgroup");
        self.write_logging_command("chown");
        self.write_logging_command("mkdir");
        self.write_logging_command("su-exec");
    }

    fn write_root_id_command(&self, fabro_groups: &str) {
        self.write_fake_command(
            "id",
            &format!(
                r#"case "$*" in
  "-u") echo 0 ;;
  "-G fabro") echo {fabro_groups} ;;
  *) echo "unexpected id args: $*" >&2; exit 64 ;;
esac
"#
            ),
        );
    }

    fn write_socket_gid_command(&self, gid: u32) {
        self.write_fake_command(
            "stat",
            &format!(
                r#"test "$1" = "-c"
test "$2" = "%g"
echo {gid}
"#
            ),
        );
    }

    fn write_fake_command(&self, name: &str, body: &str) {
        let path = self.bin_path().join(name);
        write_executable(&path, &format!("#!/bin/sh\nset -eu\n{body}"));
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "integration test creates executable shell fixtures"
)]
fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("writing fake command");
    let mut permissions = std::fs::metadata(path)
        .expect("reading fake command metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("marking fake command executable");
}
