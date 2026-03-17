#!/usr/bin/env python3
"""Generate Dockerfiles for SWE-bench environments.

Uses swebench specs to produce one Dockerfile per unique (repo, version) pair.
These get inlined into Daytona snapshot configs in workflow.toml files.
"""

import argparse
import json
import sys
from pathlib import Path

from swebench.harness.constants import MAP_REPO_VERSION_TO_SPECS


SNAPSHOT_VERSION = "v4"

# Pin Miniconda to a version that supports older Python versions (3.5, 3.6, etc.)
MINICONDA_URL = "https://repo.anaconda.com/miniconda/Miniconda3-py311_23.11.0-2-Linux-x86_64.sh"


def repo_version_key(repo: str, version: str) -> str:
    """Snapshot name for a (repo, version) pair."""
    slug = repo.replace("/", "-").replace("_", "-").lower()
    return f"swebench-{slug}-{version}-{SNAPSHOT_VERSION}"


def generate_dockerfile(repo: str, version: str) -> str:
    """Generate a Dockerfile for a (repo, version) environment.

    Installs system deps, miniconda, creates a testbed conda env with the
    correct Python version and pip packages from the swebench spec.
    """
    specs = MAP_REPO_VERSION_TO_SPECS.get(repo, {}).get(version, {})
    python_version = specs.get("python", "3.9")

    # System packages
    system_packages = [
        "git", "curl", "build-essential", "ripgrep", "ca-certificates",
        "wget", "pkg-config", "libffi-dev", "libssl-dev",
    ]

    # Conda packages — skip file references (requirements.txt, environment.yml)
    # which refer to repo files not available at Docker build time
    conda_packages = specs.get("packages", "")
    if conda_packages in ("requirements.txt", "environment.yml"):
        conda_packages = ""
    pip_packages = specs.get("pip_packages", [])
    if isinstance(pip_packages, str):
        pip_packages = [pip_packages] if pip_packages else []

    lines = [
        "FROM ubuntu:22.04",
        "",
        "ENV DEBIAN_FRONTEND=noninteractive",
        "RUN apt-get update && apt-get install -y --no-install-recommends \\",
        "    " + " \\\n    ".join(system_packages) + " \\",
        "    && rm -rf /var/lib/apt/lists/*",
        "",
    ]

    # Miniconda
    lines.extend([
        f"RUN curl -sL {MINICONDA_URL} -o /tmp/mc.sh && \\",
        "    bash /tmp/mc.sh -b -p /opt/miniconda3 && \\",
        "    rm /tmp/mc.sh",
        "",
        "ENV PATH=/opt/miniconda3/envs/testbed/bin:/opt/miniconda3/bin:$PATH",
        "",
    ])

    # Conda env with Python version — use conda-forge for old Python versions
    # that are no longer available in the defaults channel
    channel = "-c conda-forge" if python_version in ("3.5", "3.6") else ""
    conda_create = f"RUN conda create -n testbed {channel} python={python_version} -y".replace("  ", " ")
    if conda_packages:
        conda_create += f" && conda install -n testbed -y {conda_packages}"
    lines.append(conda_create)
    lines.append("")

    # Activate testbed env for subsequent RUN commands
    lines.append("SHELL [\"bash\", \"-c\"]")
    lines.append("ENV CONDA_DEFAULT_ENV=testbed")
    lines.append("")

    # Pip packages
    if pip_packages:
        pip_str = " ".join(pip_packages)
        lines.append(f"RUN pip install {pip_str}")
        lines.append("")

    lines.append("WORKDIR /home/daytona/workspace")
    lines.append("")

    return "\n".join(lines)


def get_all_repo_versions() -> list[tuple[str, str]]:
    """Return all unique (repo, version) pairs from swebench specs."""
    pairs = []
    for repo, versions in MAP_REPO_VERSION_TO_SPECS.items():
        for version in versions:
            pairs.append((repo, version))
    return sorted(pairs)


def main():
    parser = argparse.ArgumentParser(description="Generate SWE-bench Dockerfiles")
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(__file__).parent / "dockerfiles",
        help="Directory to write Dockerfiles to",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="List all (repo, version) pairs and exit",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        dest="output_json",
        help="Output snapshot name -> dockerfile mapping as JSON",
    )
    args = parser.parse_args()

    pairs = get_all_repo_versions()

    if args.list:
        for repo, version in pairs:
            key = repo_version_key(repo, version)
            print(f"{key}: {repo} @ {version}")
        print(f"\nTotal: {len(pairs)} environments")
        return

    dockerfiles = {}
    for repo, version in pairs:
        key = repo_version_key(repo, version)
        dockerfile = generate_dockerfile(repo, version)
        dockerfiles[key] = dockerfile

    if args.output_json:
        json.dump(dockerfiles, sys.stdout, indent=2)
        print()
        return

    args.output_dir.mkdir(parents=True, exist_ok=True)
    for key, dockerfile in dockerfiles.items():
        path = args.output_dir / f"{key}.Dockerfile"
        path.write_text(dockerfile)
        print(f"Wrote {path}")

    print(f"\nGenerated {len(dockerfiles)} Dockerfiles in {args.output_dir}")


if __name__ == "__main__":
    main()
