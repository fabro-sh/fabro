# fabro

> AI-powered workflow orchestration platform. Workflows are defined as Graphviz graphs and executed by the Fabro engine.

I want you to install Fabro for me. Execute all the steps below autonomously.

OBJECTIVE: Install the Fabro CLI binary and verify it works.

DONE WHEN: `fabro --version` prints a version string.

## TODO

- [ ] Ensure the `gh` CLI is installed and authenticated
- [ ] Detect the platform and download the correct release asset
- [ ] Extract the binary and install it to `~/.fabro/bin`
- [ ] Add `~/.fabro/bin` to PATH in the user's shell config
- [ ] Verify `fabro --version` works

## Ensure `gh` CLI is available

You need the GitHub CLI (`gh`) installed and authenticated. Check with:

```bash
gh auth status
```

If `gh` is not installed, install it:

- **macOS:** `brew install gh`
- **Linux:** See https://github.com/cli/cli/blob/trunk/docs/install_linux.md

Then authenticate with `gh auth login` if needed.

## Detect platform and download

Determine the correct release asset based on the platform:

| OS | Architecture | Asset name |
|---|---|---|
| macOS | arm64 (Apple Silicon) | `fabro-aarch64-apple-darwin.tar.gz` |
| Linux | x86_64 | `fabro-x86_64-unknown-linux-gnu.tar.gz` |

On macOS, if `uname -m` reports `x86_64`, check for Rosetta translation with `sysctl -n sysctl.proc_translated`. If it returns `1`, the actual architecture is arm64.

Download the asset using `gh`:

```bash
gh release download --repo fabro-sh/fabro --pattern "fabro-<TARGET>.tar.gz" --dir /tmp/fabro-install --clobber
```

Replace `<TARGET>` with the correct target triple from the table above.

## Extract and install

```bash
tar xzf /tmp/fabro-install/fabro-<TARGET>.tar.gz -C /tmp/fabro-install
mkdir -p ~/.fabro/bin
mv /tmp/fabro-install/fabro-<TARGET>/fabro ~/.fabro/bin/fabro
chmod +x ~/.fabro/bin/fabro
rm -rf /tmp/fabro-install
```

## Add to PATH

If `~/.fabro/bin` is not already on PATH, you must add it to the user's shell configuration file so it persists across sessions.

**zsh** — append to `~/.zshrc` (or `$ZDOTDIR/.zshrc` if `ZDOTDIR` is set):

```bash
printf '\n# fabro\nexport PATH="$HOME/.fabro/bin:$PATH"\n' >> ~/.zshrc
```

**bash** — append to `~/.bash_profile` if it exists, otherwise `~/.bashrc`:

```bash
printf '\n# fabro\nexport PATH="$HOME/.fabro/bin:$PATH"\n' >> ~/.bash_profile
```

**fish** — append to `~/.config/fish/config.fish`:

```bash
printf '\n# fabro\nfish_add_path $HOME/.fabro/bin\n' >> ~/.config/fish/config.fish
```

After updating the config file, export PATH for the current session too:

```bash
export PATH="$HOME/.fabro/bin:$PATH"
```

## Verify

```bash
fabro --version
```

This must print a version string like `fabro 0.x.y`. If it does, installation is complete.

Tell the user to run `fabro install` in their terminal to complete setup.

EXECUTE NOW: Complete the above TODO list to achieve: `fabro --version` prints a version string.
