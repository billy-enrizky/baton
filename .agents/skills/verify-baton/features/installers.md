# Installers

The shell and PowerShell installers choose a release asset by OS and CPU, download `SHA256SUMS`, verify the archive, and install `baton`.

The local proof does not need the network or a real release:

```sh
bash ci/installer-selftest.sh
```

It builds a fake `baton-x86_64-unknown-linux-gnu.tar.gz`, serves it through a fake `curl`, installs into a temporary `bin`, and checks the reported version. It then corrupts the archive and requires the installer to reject it without writing a binary.

The shell installer also falls back to `cargo install --git` when no matching release asset exists and Cargo is installed. That path is not exercised by the offline test because it would compile the real project.

The release workflow checks out the requested tag, verifies that the tag exists and its `vX.Y.Z` name matches `Cargo.toml`, and only then starts the asset matrix. Publish creates a draft, uploads the five archives plus `SHA256SUMS`, verifies every checksum, and makes the draft public last. A retry may repair a draft, but repository-level immutable releases reject mutation after publication; the workflow also verifies `isImmutable` and that the released tag is the `releases/latest` result.


v0.4.0 proof: release run [45076016585](https://github.com/billy-enrizky/baton/actions/runs/45076016585) published five archives plus `SHA256SUMS`, all five checksums verified, the release is immutable and expected as `releases/latest`, tap commit `03741e5` passed strict audit, reinstall, test, version, and stable JSON before and after push, and the default curl install produced `baton 0.4.0` without a source fallback.

v0.4.1 proof: release run [45523844591](https://github.com/billy-enrizky/baton/actions/runs/45523844591) published five archives plus `SHA256SUMS`, all five checksums verified, and the release is immutable and expected as `releases/latest`. Tap commit `8ccbead` passed strict audit, upgrade, and `brew test`, and the installed binary reports `baton 0.4.1`. A clean local `install.sh` run with `BATON_VERSION=v0.4.1` produced `baton 0.4.1` without a source fallback. The PTY slash probe passed against both installed binaries. The earlier v0.4.0 Homebrew binary predates the newer commands, so a release check must compare `baton --version` before judging a command missing.

v0.4.2 proof: release run [45551789917](https://github.com/billy-enrizky/baton/actions/runs/45551789917) published five archives plus `SHA256SUMS`, all five checksums verified, and the release is immutable and expected as `releases/latest`. Tap commit `1fa3223` passed strict audit, upgrade, and `brew test`, and the installed binary reports `baton 0.4.2`. A clean local `install.sh` run with `BATON_VERSION=v0.4.2` produced `baton 0.4.2` without a source fallback. The PTY question probe passed against the Homebrew, Cargo, and curl binaries; the Homebrew binary also passed the slash probe and the 50-command sweep.

v0.4.3 proof: release run [45563843655](https://github.com/billy-enrizky/baton/actions/runs/45563843655) published five archives plus `SHA256SUMS`, all five checksums verified, and the release is immutable and expected as `releases/latest`. Tap commit `ab46981` passed strict audit and `brew test`, and the installed binary reports `baton 0.4.3`. A clean local `install.sh` run with `BATON_VERSION=v0.4.3` produced `baton 0.4.3` without a source fallback. The Homebrew binary passed the slash probe and 50-command sweep; the Codex question and Antigravity approval-plus-question probes also passed. No published release contains `/login`, `/plan`, `!`, Ctrl+K, `/btw`, `/merge-back`, or `/questions`; the next installed-binary proof must name the release that first ships them.
