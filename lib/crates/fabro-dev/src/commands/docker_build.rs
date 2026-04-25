use std::fmt;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};

use super::{PlannedCommand, run_command, shell_arg, workspace_root};

const ZIG_VERSION: &str = "0.13.0";

#[derive(Debug, Args)]
pub(crate) struct DockerBuildArgs {
    /// Target Docker architecture.
    #[arg(long, value_enum)]
    arch:         Option<DockerArch>,
    /// Docker image tag to build.
    #[arg(long, default_value = "fabro-sh/fabro")]
    tag:          String,
    /// Stage the compiled binary without running docker build.
    #[arg(long)]
    compile_only: bool,
    /// Print the Docker commands instead of running them.
    #[arg(long)]
    dry_run:      bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum DockerArch {
    Amd64,
    Arm64,
}

impl DockerArch {
    fn detect() -> Result<Self> {
        match std::env::consts::ARCH {
            "x86_64" | "amd64" => Ok(Self::Amd64),
            "aarch64" | "arm64" => Ok(Self::Arm64),
            arch => bail!("unsupported host arch: {arch}"),
        }
    }

    fn target(self) -> &'static str {
        match self {
            Self::Amd64 => "x86_64-unknown-linux-musl",
            Self::Arm64 => "aarch64-unknown-linux-musl",
        }
    }

    fn zig_arch(self) -> &'static str {
        match self {
            Self::Amd64 => "x86_64",
            Self::Arm64 => "aarch64",
        }
    }
}

impl fmt::Display for DockerArch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Amd64 => formatter.write_str("amd64"),
            Self::Arm64 => formatter.write_str("arm64"),
        }
    }
}

struct DockerBuildPlan {
    arch:           DockerArch,
    compile_only:   bool,
    tag:            String,
    workspace_root: PathBuf,
}

#[expect(
    clippy::print_stdout,
    reason = "dev docker-build command reports progress and dry-run commands directly"
)]
pub(crate) fn docker_build(args: DockerBuildArgs) -> Result<()> {
    let plan = DockerBuildPlan {
        arch:           args.arch.map_or_else(DockerArch::detect, Ok)?,
        compile_only:   args.compile_only,
        tag:            args.tag,
        workspace_root: workspace_root(),
    };

    if args.dry_run {
        for line in plan.dry_run_lines() {
            println!("{line}");
        }
        return Ok(());
    }

    plan.run()
}

impl DockerBuildPlan {
    #[expect(
        clippy::print_stdout,
        reason = "dev docker-build command reports progress directly"
    )]
    fn run(&self) -> Result<()> {
        println!(
            "Building fabro-cli for {} inside rust:1-bookworm via cargo-zigbuild...",
            self.arch.target()
        );
        let build_command = self.build_command();
        run_command(&self.workspace_root, &build_command)?;

        println!("Extracting binary from builder cache...");
        std::fs::create_dir_all(self.context_dir()).with_context(|| {
            format!(
                "creating Docker context directory {}",
                self.context_dir().display()
            )
        })?;
        let extract_command = self.extract_command();
        run_command(&self.workspace_root, &extract_command)?;

        if self.compile_only {
            println!(
                "Staged tmp/docker-context/{}/fabro (skipping docker build per --compile-only).",
                self.arch
            );
            return Ok(());
        }

        println!("Building Docker image as {}...", self.tag);
        let image_build_command = self.image_build_command();
        run_command(&self.workspace_root, &image_build_command)
    }

    fn dry_run_lines(&self) -> Vec<String> {
        let mut lines = vec![
            self.build_command().to_shell_line(),
            format!("mkdir -p {}", shell_arg(self.relative_context_dir())),
            self.extract_command().to_shell_line(),
        ];
        if self.compile_only {
            lines.push(format!("staged tmp/docker-context/{}/fabro", self.arch));
        } else {
            lines.push(self.image_build_command().to_shell_line());
        }
        lines
    }

    fn build_command(&self) -> PlannedCommand {
        let arch = self.arch.to_string();
        let target = self.arch.target();
        let zig_arch = self.arch.zig_arch();
        PlannedCommand::new("docker")
            .arg("run")
            .arg("--rm")
            .arg("--platform")
            .arg(format!("linux/{arch}"))
            .arg("-v")
            .arg(format!("{}:/src", self.workspace_root.display()))
            .arg("-v")
            .arg("fabro-docker-cargo-registry:/usr/local/cargo/registry")
            .arg("-v")
            .arg(format!("fabro-docker-cargo-target-{arch}:/target"))
            .arg("-v")
            .arg(format!("fabro-docker-zig-{arch}:/opt/zig"))
            .arg("-v")
            .arg(format!("fabro-docker-cargo-tools-{arch}:/opt/cargo-tools"))
            .arg("-w")
            .arg("/src")
            .arg("-e")
            .arg("CARGO_TARGET_DIR=/target")
            .arg("-e")
            .arg("LIBZ_SYS_STATIC=1")
            .arg("rust:1-bookworm")
            .arg("bash")
            .arg("-c")
            .arg(build_script(target, zig_arch))
    }

    fn extract_command(&self) -> PlannedCommand {
        let arch = self.arch.to_string();
        PlannedCommand::new("docker")
            .arg("run")
            .arg("--rm")
            .arg("--platform")
            .arg(format!("linux/{arch}"))
            .arg("-v")
            .arg(format!("fabro-docker-cargo-target-{arch}:/target"))
            .arg("-v")
            .arg(format!("{}:/out", self.context_dir().display()))
            .arg("rust:1-bookworm")
            .arg("cp")
            .arg(format!("/target/{}/release/fabro", self.arch.target()))
            .arg("/out/fabro")
    }

    fn image_build_command(&self) -> PlannedCommand {
        PlannedCommand::new("docker")
            .arg("build")
            .arg("--platform")
            .arg(format!("linux/{}", self.arch))
            .arg("-t")
            .arg(&self.tag)
            .arg(".")
    }

    fn context_dir(&self) -> PathBuf {
        self.workspace_root
            .join("tmp")
            .join("docker-context")
            .join(self.arch.to_string())
    }

    fn relative_context_dir(&self) -> String {
        format!("tmp/docker-context/{}", self.arch)
    }
}

fn build_script(target: &str, zig_arch: &str) -> String {
    format!(
        "set -e; \
         apt-get update -qq && apt-get install -y -qq pkg-config perl make cmake xz-utils curl >/dev/null; \
         if [ ! -x /opt/zig/zig-linux-{zig_arch}-{ZIG_VERSION}/zig ]; then \
         curl -fsSL https://ziglang.org/download/{ZIG_VERSION}/zig-linux-{zig_arch}-{ZIG_VERSION}.tar.xz | tar -xJ -C /opt/zig; \
         fi; \
         export PATH=/opt/cargo-tools/bin:/opt/zig/zig-linux-{zig_arch}-{ZIG_VERSION}:$PATH; \
         if ! command -v cargo-zigbuild >/dev/null; then \
         cargo install --locked --root /opt/cargo-tools cargo-zigbuild; \
         fi; \
         rustup target add {target}; \
         cargo zigbuild --release -p fabro-cli --target {target}"
    )
}
