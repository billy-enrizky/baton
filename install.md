# Install Baton

Baton ships as a single binary in release archives for x86_64 and aarch64 macOS and Linux, plus x86_64 Windows. The harness CLIs you want to drive are separate programs and must be on `PATH`. A harness is the coding agent CLI you already use, such as Codex, Claude Code, Cursor, Grok, OpenCode, or Antigravity.

## macOS and Linux

```sh
curl -fsSL https://raw.githubusercontent.com/billy-enrizky/baton/main/install.sh | sh
```

The script installs to `~/.local/bin` by default. Set `BATON_INSTALL_DIR` to another directory whose final component is `bin`, or `BATON_VERSION` to pin a release:

```sh
BATON_VERSION=v0.4.3 BATON_INSTALL_DIR="$HOME/bin" \
  sh -c "$(curl -fsSL https://raw.githubusercontent.com/billy-enrizky/baton/main/install.sh)"
```

If `~/.local/bin` is not on `PATH`, the script prints the export line to add. The shell installer needs `tar` or `unzip`, and `sha256sum`, `shasum`, or `openssl` to verify the archive.

## Windows

```powershell
irm https://raw.githubusercontent.com/billy-enrizky/baton/main/install.ps1 | iex
```

The script installs to `%LOCALAPPDATA%\Baton\bin` by default and adds it to your user `PATH`. Set `BATON_INSTALL_DIR` or `BATON_VERSION` before running it to change the directory or pin a release. It uses the built-in `Expand-Archive` command, available in PowerShell 5 and newer.

Windows on ARM is not published yet. Use an x64 Windows machine, or build from a source checkout with `cargo install --path . --locked`.

## Homebrew

```sh
brew install billy-enrizky/baton/baton
```

Upgrade with `brew upgrade billy-enrizky/baton/baton`. The formula is maintained in the `billy-enrizky/baton` tap, outside this repository.

## From source

```sh
git clone https://github.com/billy-enrizky/baton.git
cd baton
cargo install --path . --locked
baton --version
```

The source install goes to Cargo's bin directory, usually `~/.cargo/bin`.

## What the installer checks

The release installer downloads the archive for the current OS and CPU, verifies its SHA-256 against `SHA256SUMS`, extracts the `baton` binary (`baton.exe` on Windows), and prints the installed version. When the archive is unavailable, it falls back to a locked source build when Cargo and git are installed. A checksum mismatch stops the install.

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

`command -v baton` names the binary that will run. When a Homebrew install and a Cargo install both exist, the earlier `PATH` entry wins. Run `baton --version` when a command seems missing, because an older install will not have the newer commands.

## Requirements

- One or more harness CLIs on `PATH`: `codex`, `claude`, `cursor-agent`, `grok`, `opencode`, or `agy`.
- Git for checkpoints, worktrees, and `/git`.
- `VISUAL` or `EDITOR` for Ctrl+O in `/files` and `/grep`, and for `/memory`.
- Optional: `rg` for faster file and content search. Without it, `/files` uses `git ls-files` and `/grep` uses `git grep`.
- Optional: authenticated `gh` for `/git checks`, `/git pr`, `/git review`, `/git merge`, `/task`, and opening pull requests from orchestration reports.

## Upgrade

Re-run the installer, run `brew upgrade billy-enrizky/baton/baton`, run `baton update` from a source checkout, or run `cargo install --path . --locked`. `baton doctor` will report the new version on the next start.

## Uninstall

Remove the binary from the install location:

```sh
rm ~/.local/bin/baton    # curl installer
brew uninstall billy-enrizky/baton/baton     # Homebrew
cargo uninstall baton    # source install
```

On Windows, remove `%LOCALAPPDATA%\Baton\bin\baton.exe` and remove `%LOCALAPPDATA%\Baton\bin` from your user `PATH` if the installer added it.

Data lives in `~/.baton` (`%USERPROFILE%\.baton` on Windows) unless `BATON_HOME` is set. Remove that directory only when you also want to delete threads, configuration, and the checkpoint index.

Checkpoint snapshots are hidden Git refs under `refs/baton/checkpoints/` in each project repository, not files under `~/.baton`. Deleting the Baton data directory leaves those refs. To remove them from a repository:

```sh
git -C <repository> for-each-ref --format='delete %(refname)' refs/baton/checkpoints \
  | git -C <repository> update-ref --stdin
```

Review the ref list first with `git -C <repository> for-each-ref refs/baton/checkpoints` if you want to keep some snapshots.

## Next

[Set up Baton](setup.md) for provider login, API keys, custom endpoints, roles, MCP, skills, and memory.
