# Builds and releases

`Dockerfile.stagex` builds static Linux x86-64 resolver binaries for mainnet and
testnet using digest-pinned StageX Rust, protobuf, and Abseil images. Dependencies
come from the committed `Cargo.lock`; compilation and smoke checks run without
external networking. The StageX compiler is supplied by the pinned Rust pallet,
independently of the development toolchain in `rust-toolchain.toml`.

## Build locally

Docker with Buildx is required. On an ARM Mac, Docker must support Linux x86-64
emulation.

```sh
docker buildx build --platform linux/amd64 \
  --file Dockerfile.stagex --target binaries \
  --output type=local,dest=dist .
```

The build exports:

- `dist/zns-resolver-mainnet-x86_64-linux-musl`
- `dist/zns-resolver-testnet-x86_64-linux-musl`

Before export, the build checks the ELF architecture, absence of an interpreter
or shared-library dependencies, database initialization, the network-specific
`status` response, and an empty-database `resolve` request. This is a startup
check, not a chain synchronization or load test.

## GitHub Actions

The **Release** workflow runs build checks on relevant pull requests and
pushes to `master`. A manual run builds and uploads workflow artifacts without
publishing a release. Builds run on native x86-64 runners.

To publish, first merge the release changes into `master`, including the desired
package version in `Cargo.toml` and its corresponding `Cargo.lock` update. Push a
tag matching that package version exactly:

```sh
git tag v0.0.3
git push origin v0.0.3
```

`v0.0.3` is an example; use the version in the tagged commit. Prerelease versions
such as `0.0.3-rc.1` use `v0.0.3-rc.1` and are marked as prereleases.

The workflow builds and smoke-tests both networks, then creates a GitHub Release
containing both executables, `SHA256SUMS`, and `BUILD-INFO.txt` (the source commit,
ref, package version, target, and pinned StageX images). Publishing uses the
workflow's `GITHUB_TOKEN`; no registry credentials or container image publishing
are needed. An existing release is not overwritten; publish a new version for
changed artifacts.

## Download and verify

```sh
gh release download v0.0.3 --repo zcashme/zns-resolver --dir release
cd release
sha256sum --check SHA256SUMS
chmod +x zns-resolver-*-x86_64-linux-musl
```

On macOS, use `shasum -a 256 --check SHA256SUMS`. These executables run on Linux,
not directly on macOS. Checksums detect differences from the downloaded checksum
file; they do not independently authenticate its publisher.

## Use in an SEV-SNP guest

These releases supply application binaries for guest-image assembly. They do not
yet include a bootable guest, kernel, firmware, TLS/attestation gateway, expected
SNP measurement, or client verifier. Package both binaries into the guest's
verified boot contents and keep their databases separate. Their current internal
RPC addresses are `127.0.0.1:3031` (mainnet) and `127.0.0.1:8080` (testnet).

StageX supplies pinned build inputs. Byte-for-byte reproducibility of these
application binaries has not yet been established by independent rebuilds, and
a binary checksum is not a hardware attestation of a running guest.
