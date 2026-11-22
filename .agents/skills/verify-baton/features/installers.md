# Installers

The shell and PowerShell installers choose a release asset by OS and CPU, download `SHA256SUMS`, verify the archive, and install `baton`.

The local proof does not need the network or a real release:

```sh
bash ci/installer-selftest.sh
```

It builds a fake `baton-x86_64-unknown-linux-gnu.tar.gz`, serves it through a fake `curl`, installs into a temporary `bin`, and checks the reported version. It then corrupts the archive and requires the installer to reject it without writing a binary.

The shell installer also falls back to `cargo install --git` when no matching release asset exists and Cargo is installed. That path is not exercised by the offline test because it would compile the real project.

The release workflow checks out the requested tag, verifies that the tag exists and its `vX.Y.Z` name matches `Cargo.toml`, and only then starts the asset matrix. Publish creates a draft, uploads the five archives plus `SHA256SUMS`, verifies every checksum, and makes the draft public last. A retry may repair a draft, but repository-level immutable releases reject mutation after publication; the workflow also verifies `isImmutable` and that the released tag is the `releases/latest` result.
