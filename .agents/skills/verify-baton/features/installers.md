# Installers

The shell and PowerShell installers choose a release asset by OS and CPU, download `SHA256SUMS`, verify the archive, and install `baton`.

The local proof does not need the network or a real release:

```sh
bash ci/installer-selftest.sh
```

It builds a fake `baton-x86_64-unknown-linux-gnu.tar.gz`, serves it through a fake `curl`, installs into a temporary `bin`, and checks the reported version. It then corrupts the archive and requires the installer to reject it without writing a binary.

The shell installer also falls back to `cargo install --git` when no matching release asset exists and Cargo is installed. That path is not exercised by the offline test because it would compile the real project.
