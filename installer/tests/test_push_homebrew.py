"""Exercise publication against local Git repositories; no network or credentials."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "push-homebrew.sh"
REAL_GIT = shutil.which("git")


class HomebrewPushTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.tap = self.root / "tap"
        self.remote = self.root / "remote.git"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.env = {
            **os.environ,
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_TERMINAL_PROMPT": "0",
            "REAL_GIT": REAL_GIT,
            "TEST_ROOT": str(self.root),
        }
        for key in list(self.env):
            if key.startswith(("GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_")) or key in (
                "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_CONFIG_COUNT"
            ):
                del self.env[key]
        self.git("init", "--bare", "--initial-branch=main", str(self.remote))
        self.git("init", "--initial-branch=main", str(self.tap))
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")
        self.git("remote", "add", "origin", str(self.remote))
        self.commit("old version")
        self.git("push", "origin", "HEAD:main")
        self.before = self.git("rev-parse", "HEAD")
        self.commit("new version")
        self.expected = self.git("rev-parse", "HEAD")
        # Keep tests fast and record the backoff without changing production code.
        self.write_executable(self.bin / "sleep", '#!/bin/sh\necho "$1" >> "$TEST_ROOT/delays"\n')
        self.write_executable(self.bin / "git", '''#!/bin/sh
printf 'attempt\n' >> "$TEST_ROOT/attempts"
"$REAL_GIT" "$@"
status=$?
if [ "$status" -eq 0 ] && [ -f "$TEST_ROOT/lose-response" ]; then
  rm "$TEST_ROOT/lose-response"
  exit 1
fi
exit "$status"
''')

    def git(self, *args):
        result = subprocess.run(
            [REAL_GIT, *args], cwd=self.tap if self.tap.exists() else self.root,
            env=self.env, text=True, capture_output=True, check=True,
        )
        return result.stdout.strip()

    def commit(self, value):
        (self.tap / "formula").write_text(value)
        self.git("add", "formula")
        self.git("commit", "-m", value)

    def write_executable(self, path, content):
        path.write_text(content)
        path.chmod(0o755)

    def reject_pushes(self, failures):
        self.write_executable(self.remote / "hooks" / "pre-receive", f'''#!/bin/sh
count=0
if [ -f reject-count ]; then count=$(cat reject-count); fi
count=$((count + 1))
echo "$count" > reject-count
if [ "$count" -le {failures} ]; then
  echo "Internal Server Error" >&2
  exit 1
fi
''')

    def run_push(self, success, attempts, delays):
        result = subprocess.run(
            ["bash", str(SCRIPT)], cwd=self.tap, text=True, capture_output=True,
            env={**self.env, "PATH": str(self.bin) + os.pathsep + os.environ["PATH"]},
            timeout=10,
        )
        self.assertEqual(result.returncode == 0, success, result.stderr)
        self.assertEqual((self.root / "attempts").read_text().splitlines(), ["attempt"] * attempts)
        delay_file = self.root / "delays"
        self.assertEqual(delay_file.read_text().splitlines() if delay_file.exists() else [], delays)
        return self.git("--git-dir", str(self.remote), "rev-parse", "main")

    def test_success_needs_no_retry(self):
        self.assertEqual(self.run_push(True, 1, []), self.expected)

    def test_transient_server_rejection_recovers(self):
        self.reject_pushes(2)
        self.assertEqual(self.run_push(True, 3, ["10", "30"]), self.expected)

    def test_persistent_failure_remains_failure(self):
        self.reject_pushes(99)
        self.assertEqual(self.run_push(False, 3, ["10", "30"]), self.before)

    def test_lost_success_response_is_idempotent(self):
        (self.root / "lose-response").touch()
        self.assertEqual(self.run_push(True, 2, ["10"]), self.expected)
        self.assertEqual(self.git("rev-parse", "HEAD"), self.expected)

    def test_already_published_commit_is_noop(self):
        self.git("push", "origin", "HEAD:main")
        self.assertEqual(self.run_push(True, 1, []), self.expected)

    def test_concurrent_publication_is_not_overwritten(self):
        self.git("checkout", "--detach", self.before)
        self.commit("competing version")
        competing = self.git("rev-parse", "HEAD")
        self.git("push", "origin", "HEAD:main")
        self.git("checkout", "main")
        self.assertEqual(self.run_push(False, 3, ["10", "30"]), competing)
        self.assertEqual(self.git("rev-parse", "HEAD"), self.expected)


if __name__ == "__main__":
    unittest.main()
