#!/usr/bin/env python3
"""Exercise a release bundle without developer plugin paths or checksum overrides."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import tarfile
import time
import urllib.request


def workflow(fabro, root, expect_tampered=False, after_first_run=None):
    root.mkdir()
    config = root / "settings.toml"
    config.write_text(
        '_version = 1\n[server.auth]\nmethods = ["dev-token"]\n'
        f'[server.storage]\nroot = "{root / "storage"}"\n'
    )
    graph = root / "workflow.fabro"
    graph.write_text(
        'digraph smoke { start [shape=Mdiamond]; exit [shape=Msquare]; '
        'check [shape=parallelogram, script="printf bundle-smoke-ok"]; '
        'start -> check -> exit; }\n'
    )
    (root / "workflow.toml").write_text(
        '_version = 1\n[workflow]\ngraph = "workflow.fabro"\n'
        '[run.environment]\nid = "local"\n'
    )
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    server_url = f"http://127.0.0.1:{port}"
    token = "fabro_dev_" + "ab" * 32
    # Keep credentials, proxy settings and developer checksum overrides out.
    env = {
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
        "FABRO_HOME": str(root / "home"),
        "FABRO_SERVER": server_url,
        "FABRO_DEV_TOKEN": token,
        "SESSION_SECRET": "0123456789abcdef" * 4,
        "FABRO_NO_UPGRADE_CHECK": "true",
        "FABRO_TELEMETRY": "off",
        "FABRO_SUPPRESS_OPEN_BROWSER": "1",
        "FABRO_HTTP_PROXY_POLICY": "disabled",
        "NO_COLOR": "1",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
    }

    def run(*args):
        return subprocess.run(
            [str(fabro), *args], cwd=root, env=env,
            capture_output=True, text=True, timeout=120,
        )

    log_path = root / "server.log"
    with log_path.open("w") as log:
        server = subprocess.Popen(
            [str(fabro), "server", "start", "--foreground", "--no-web",
             "--config", str(config), "--bind", f"127.0.0.1:{port}"],
            cwd=root, env=env, stdout=log, stderr=log, start_new_session=True,
        )
        try:
            client = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            deadline = time.monotonic() + 60
            while True:
                try:
                    with client.open(server_url + "/health", timeout=1) as response:
                        if response.status == 200:
                            break
                except OSError:
                    # Startup connection failures are expected; retry until the deadline.
                    pass
                if server.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("server did not start:\n" + log_path.read_text())
                time.sleep(0.1)

            login = run("auth", "login", "--dev-token", token)
            if login.returncode:
                raise RuntimeError(login.stdout + login.stderr)
            result = run("run", str(root / "workflow.toml"), "--environment", "local", "--auto-approve")
            transcript = result.stdout + result.stderr
            if expect_tampered:
                if result.returncode == 0 or "checksum" not in transcript.lower():
                    raise RuntimeError("tampered plugin was not rejected for its checksum:\n" + transcript)
            elif result.returncode:
                raise RuntimeError("packaged workflow failed:\n" + transcript + log_path.read_text())
            if after_first_run:
                after_first_run()
                result = run("run", str(root / "workflow.toml"), "--environment", "local", "--auto-approve")
                if result.returncode:
                    raise RuntimeError("running server lost its original plugin bundle after an upgrade:\n" + result.stdout + result.stderr)
                graph_result = run("graph", str(root / "workflow.toml"))
                if graph_result.returncode or "<svg" not in graph_result.stdout:
                    raise RuntimeError("running server lost its graph renderer after an upgrade:\n" + graph_result.stdout + graph_result.stderr)
        finally:
            # Terminate only the process group created for this isolated server.
            try:
                os.killpg(server.pid, signal.SIGTERM)
            except ProcessLookupError:
                # The server's process group may already have exited during teardown.
                pass
            try:
                server.wait(timeout=15)
            except subprocess.TimeoutExpired:
                os.killpg(server.pid, signal.SIGKILL)
                server.wait(timeout=15)

    if list((root / "storage").glob(".server-bundle-*")):
        raise RuntimeError("server left its private executable bundle behind after shutdown")


def upgrade_bundle(source, root, target):
    """Exercise the real updater through local, checksummed release assets."""
    assets = root / "assets"
    assets.mkdir()
    archive = assets / f"fabro-{target}.tar.gz"
    with tarfile.open(archive, "w:gz") as output:
        output.add(source, arcname=f"fabro-{target}")
    digest = hashlib.sha256()
    with archive.open("rb") as input_file:
        for chunk in iter(lambda: input_file.read(1024 * 1024), b""):
            digest.update(chunk)
    checksum = assets / (archive.name + ".sha256")
    checksum.write_text(f"{digest.hexdigest()}  {archive.name}\n")
    fake_bin = root / "fake-bin"
    fake_bin.mkdir()
    gh = fake_bin / "gh"
    gh.write_text('''#!/bin/sh
set -eu
case "$1" in
  --version) echo 'gh version test';;
  auth) exit 0;;
  api)
    case "$2" in
      repos/fabro-sh/fabro/releases/latest) echo "${FAKE_LATEST_STABLE:?unexpected latest-release lookup}";;
      repos/fabro-sh/fabro/releases) echo "${FAKE_PRERELEASES:?unexpected prerelease lookup}";;
      *) exit 1;;
    esac;;
  release)
    test "$2" = download
    test "$3" = "v$FAKE_INSTALLED_VERSION"
    asset=''; destination=''
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --pattern) asset="$2"; shift;;
        --dir) destination="$2"; shift;;
      esac
      shift
    done
    cp "$FAKE_RELEASE_DIR/$asset" "$destination/$asset";;
  *) exit 1;;
esac
''')
    gh.chmod(0o755)
    if target.endswith("-musl"):
        # Release CI runs static musl executables on glibc hosts. Model the
        # destination distro so the updater selects the musl release asset.
        ldd = fake_bin / "ldd"
        ldd.write_text("#!/bin/sh\necho musl\n")
        ldd.chmod(0o755)
    install = root / "install"
    install.mkdir()
    launcher = install / "fabro"
    # A flat executable models what the old, binary-only updater leaves behind.
    shutil.copy2(source / "fabro", launcher)
    env = {
        "PATH": f"{fake_bin}:/usr/bin:/bin:/usr/sbin:/sbin",
        "FAKE_RELEASE_DIR": str(assets),
        "FABRO_HOME": str(root / "upgrade-home"),
        "FABRO_TELEMETRY": "off",
        "FABRO_NO_UPGRADE_CHECK": "true",
        "NO_COLOR": "1",
    }
    version = subprocess.check_output([str(launcher), "--version"], env=env, text=True).split()[1]
    env["FAKE_INSTALLED_VERSION"] = version

    # Exercise the shipped shell installer with real release executables, then
    # run a workflow from the resulting launcher in a clean environment.
    clean_install = root / "clean-install"
    installer = Path(__file__).resolve().parents[2] / "apps/marketing/public/install.sh"
    result = subprocess.run(
        ["/bin/sh", str(installer)], env={
            **env, "HOME": str(root / "installer-home"),
            "FABRO_INSTALL_DIR": str(clean_install), "FAKE_LATEST_STABLE": f"v{version}",
        }, capture_output=True, text=True, timeout=120,
    )
    if result.returncode or not (clean_install / "fabro").is_symlink():
        raise RuntimeError("clean bundle installation failed:\n" + result.stdout + result.stderr)
    workflow(clean_install / "fabro", root / "installed")

    doctor = subprocess.run(
        [str(launcher), "doctor", "--json", "--server", str(root / "absent.sock")],
        env=env, capture_output=True, text=True, timeout=30,
    )
    if "`fabro upgrade`" not in doctor.stdout or "--force" in doctor.stdout:
        raise RuntimeError("incomplete installation did not explain how to finish the upgrade:\n" + doctor.stdout + doctor.stderr)

    def upgrade(*args, extra_env=None, executable=launcher):
        return subprocess.run(
            [str(executable), "upgrade", *args],
            env={**env, **(extra_env or {})}, capture_output=True, text=True, timeout=120,
        )

    # No latest-release response is available: automatic repair must select the
    # installed release, including a nightly, without consulting another channel.
    result = upgrade("--dry-run")
    if result.returncode or f"Would repair the fabro {version}" not in result.stderr:
        raise RuntimeError("repair preview did not stay on the installed version:\n" + result.stdout + result.stderr)
    if launcher.is_symlink() or list(install.iterdir()) != [launcher]:
        raise RuntimeError("dry-run changed the incomplete installation")

    # Explicit selection must still take precedence over implicit repair.
    for args, extra_env, expected in [
        (("--dry-run", "--json"), {}, version),
        (("--dry-run", "--json", "--version", "999.0.0"), {}, "999.0.0"),
        (("--dry-run", "--json", "--prerelease"), {
            "FAKE_PRERELEASES": '[{"tag_name":"v999.0.1-nightly.0","draft":false}]',
        }, "999.0.1-nightly.0"),
    ]:
        result = upgrade(*args, extra_env=extra_env)
        if result.returncode or json.loads(result.stdout) != {
            "previous_version": version, "installed_version": expected, "dry_run": True,
        }:
            raise RuntimeError("upgrade selected the wrong release:\n" + result.stdout + result.stderr)

    previous = None
    for attempt in range(2):
        result = upgrade()
        if result.returncode:
            raise RuntimeError("bundle upgrade failed:\n" + result.stdout + result.stderr)
        current = launcher.resolve()
        if not launcher.is_symlink() or current == previous:
            raise RuntimeError("upgrade did not activate a new complete bundle")
        for name in ("fabro", "sandbox-driver-host", "sandbox-driver-docker", "sandbox-driver-daytona"):
            if (current.parent / name).read_bytes() != (source / name).read_bytes():
                raise RuntimeError(f"upgrade did not preserve {name}")
            if previous:
                old = previous.parent / name
                if name == "sandbox-driver-daytona":
                    if old.exists():
                        raise RuntimeError("repair modified the old incomplete bundle")
                elif old.read_bytes() != (source / name).read_bytes():
                    raise RuntimeError(f"upgrade changed the previous bundle's {name}")
        previous = current
        # Once complete, the same version is a no-op, even if selected through
        # the default release lookup. No force is needed for either repair.
        result = upgrade(extra_env={"FAKE_LATEST_STABLE": f"v{version}"})
        if result.returncode or f"Already on version {version}" not in result.stderr or launcher.resolve() != current:
            raise RuntimeError("complete same-version install was not a no-op:\n" + result.stdout + result.stderr)
        if attempt == 0:
            # Also cover partial bundles left in managed version directories.
            (current.parent / "sandbox-driver-daytona").unlink()

    # Explicit same-version selection also repairs without --force. Refusing a
    # bad checksum must leave even an incomplete active installation untouched.
    missing = previous.parent / "sandbox-driver-daytona"
    missing.unlink()

    checksum.write_text("0" * 64 + "\n")
    result = upgrade()
    if result.returncode == 0 or "sha256 mismatch" not in result.stderr.lower():
        raise RuntimeError("updater accepted an invalid archive checksum:\n" + result.stdout + result.stderr)
    if launcher.resolve() != previous:
        raise RuntimeError("failed upgrade changed the active bundle")
    checksum.write_text(f"{digest.hexdigest()}  {archive.name}\n")
    result = upgrade("--version", version)
    if result.returncode or not (launcher.resolve().parent / missing.name).is_file():
        raise RuntimeError("explicit same-version repair failed:\n" + result.stdout + result.stderr)

    def upgrade_and_tamper_new_bundle(executable=launcher):
        result = upgrade("--force", "--version", version, executable=executable)
        if result.returncode:
            raise RuntimeError(result.stdout + result.stderr)
        with (executable.resolve().parent / "sandbox-driver-host").open("ab") as plugin:
            plugin.write(b"\nchecksum smoke test\n")

    # The first upgrade from a manually extracted, flat release must preserve
    # worker identity too, before the server has ever entered a managed bundle.
    flat_install = root / "flat-install"
    shutil.copytree(source, flat_install)
    flat_launcher = flat_install / "fabro"
    workflow(
        flat_launcher, root / "flat-upgraded",
        after_first_run=lambda: upgrade_and_tamper_new_bundle(flat_launcher),
    )
    workflow(flat_launcher, root / "flat-tampered", expect_tampered=True)

    return launcher, upgrade_and_tamper_new_bundle


def main():
    source = Path(sys.argv[1]).resolve().parent
    with tempfile.TemporaryDirectory(prefix="fabro-bundle-smoke-") as directory:
        root = Path(directory)
        bundle = root / "bundle"
        bundle.mkdir()
        for name in ("fabro", "sandbox-driver-host", "sandbox-driver-docker", "sandbox-driver-daytona"):
            shutil.copy2(source / name, bundle / name)
        workflow(bundle / "fabro", root / "valid")
        launcher, upgrade_and_tamper = upgrade_bundle(bundle, root, sys.argv[2])
        # A running server must stay on its old bundle even if the newly
        # activated bundle's plugin becomes corrupt. A fresh server must reject it.
        workflow(launcher, root / "upgraded", after_first_run=upgrade_and_tamper)
        workflow(launcher, root / "tampered", expect_tampered=True)
    print("Release bundle installs cleanly, repairs missing executables, upgrades atomically, executes workflows, and rejects checksum mismatches.")


if __name__ == "__main__":
    main()
