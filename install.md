# Install Baton

Baton ships as a single binary for macOS and Linux, plus a Windows x64 archive. The harness CLIs you want to drive are separate programs and must be on `PATH`.

## macOS and Linux

```sh
curl -fsSL https://raw.githubusercontent.com/billy-enrizky/baton/main/install.sh | sh
```

The script installs to `~/.local/bin` by default. Set `BATON_INSTALL_DIR` to another bin directory, or `BATON_VERSION` to pin a release:

```sh
BATON_VERSION=v0.4.0 BATON_INSTALL_DIR="$HOME/bin" \
  sh -c "$(curl -fsSL https://raw.githubusercontent.com/billy-enrizky/baton/main/install.sh)"
```

If `~/.local/bin` is not on `PATH`, the script prints the export line to add. GNU `sha256sum`, `shasum`, or `openssl` is required to verify the archive.

## Windows

```powershell
irm https://raw.githubusercontent.com/billy-enrizky/baton/main/install.ps1 | iex
```

Windows on ARM is not published yet. Use an x64 Windows machine, or build from source.

## Homebrew

```sh
brew install billy-enrizky/baton/baton
```

Upgrade with `brew upgrade billy-enrizky/baton/baton`.

## From source

```sh
git clone https://github.com/billy-enrizky/baton.git
cd baton
cargo install --path . --locked
baton --version
```

The source install goes to Cargo's bin directory, usually `~/.cargo/bin`.

## What the installer checks

The release installer downloads the archive for the current OS and CPU, verifies its SHA-256 against `SHA256SUMS`, extracts `baton`, and prints the installed version. When no release asset matches, it falls back to a locked source build when Cargo and git are installed. A checksum mismatch stops the install.

## Verify

```sh
command -v baton
baton --version
baton doctor
```

PowerShell:

```powershell
Get-Command baton
baton --version
baton doctor
```

`baton doctor` reports each harness binary, auth readiness, config files, database size, thread count, and tracked provider processes.

## Requirements

- One or more harness CLIs on `PATH`: `codex`, `claude`, `cursor-agent`, `grok`, `opencode`, or `agy`.
- Git for checkpoints, worktrees, and `/git`.
- `VISUAL` or `EDITOR` for Ctrl+O in `/files` and `/grep`, and for `/memory`.
- Optional: `rg` for faster file and content search. Baton falls back to `git`.
- Optional: `gh` for `/git` checks and opening pull requests from orchestration reports.

## Upgrade

Re-run the installer, run `brew upgrade`, or run `cargo install --path . --locked` in a source checkout. `baton doctor` will report the new version on the next start.

## Uninstall

Remove the binary from the install location:

```sh
rm ~/.local/bin/baton    # curl installer
brew uninstall baton     # Homebrew
cargo uninstall baton    # source install
```

Data lives in `~/.baton` unless `BATON_HOME` is set. Remove that directory only when you also want to delete threads, configuration, and the checkpoint index.

Checkpoint snapshots are hidden Git refs under `refs/baton/checkpoints/` in each project repository, not files under `~/.baton`. Deleting the Baton data directory leaves those refs. To remove them from a repository:

```sh
git -C <repository> for-each-ref --format='delete %(refname)' refs/baton/checkpoints \
  | git -C <repository> update-ref --stdin
```

Review the ref list first with `git -C <repository> for-each-ref refs/baton/checkpoints` if you want to keep some snapshots.

## Next

[Set up Baton](setup.md) for provider login, API keys, custom endpoints, roles, MCP, skills, and memory.
