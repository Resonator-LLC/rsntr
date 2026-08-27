# npm packaging for rsntr

Makes `npx rsntr` work without a Rust toolchain, which is the point: most
agent sandboxes and CI images have node, few have cargo.

## Layout

    npm/rsntr/          root package, checked in (version is a placeholder)
    npm/build.mjs       assembles publishable packages into npm/dist/
    npm/dist/           generated, gitignored
    npm/assets/         downloaded release archives, gitignored

Six packages are published per release:

| package | contents |
|---|---|
| `rsntr` | the launcher shim, ~14KB |
| `rsntr-linux-x64` | glibc 2.35+ binary, x86_64 |
| `rsntr-linux-arm64` | glibc 2.35+ binary, aarch64 |
| `rsntr-darwin-x64` | Mach-O x86_64 |
| `rsntr-darwin-arm64` | Mach-O arm64 |
| `rsntr-win32-x64` | PE32+ x86-64 |

The platform packages declare `os` and `cpu`. npm refuses to install a
package whose fields do not match the host, so the root package can list all
five as `optionalDependencies` and each user downloads exactly one (~18MB)
rather than all five (~90MB).

The Linux binaries are glibc builds with a 2.35 floor (Ubuntu 22.04,
Debian 12, and newer). Static musl builds would be nicer, but musl's
4-aligned `cmsghdr` trips a cmsg alignment assert in the QUIC UDP layer
on the first received packet; until that is fixed upstream, musl-only
hosts (Alpine) are not served.

## Why the binaries are bundled

The common alternative is a postinstall script that downloads the binary
from GitHub Releases, which keeps the tarballs small. It also breaks in
offline and network-restricted sandboxes, which is where the agents this
channel exists for actually run. At ~18MB per platform, bundling is the
cheaper trade.

## Releasing

Publishing is automatic: `.github/workflows/npm.yml` runs when a GitHub
release is published, and the release workflow only publishes once every
platform has built. To run it by hand:

    node npm/build.mjs --version 0.1.0        # fetches and verifies assets
    node npm/build.mjs --version 0.1.0 --assets ./some-dir

Every archive is checked against its published `.sha256` before being
unpacked. Platform packages publish before the root package, because the
root pins exact optional dependency versions -- publishing it first would
leave a window where `npm install rsntr` resolves and then cannot find a
binary.

## Requirements

The workflow needs an `NPM_TOKEN` repository secret with publish rights
(an automation token, so 2FA does not block CI).
