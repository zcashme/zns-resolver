# HTTPS resolver guest

The Release workflow publishes `zns-resolver-guest-x86_64.tar.gz` alongside the
static binaries. This archive includes the kernel, SNP firmware, initramfs,
systemd unit, checksums, launch inputs, and expected SNP launch measurement.
Resolver logic and the JSON-RPC API are unchanged. There is no attestation
endpoint or external verifier in this version; status output is not attestation.

Both resolvers share one guest with 2 vCPUs and 4 GiB RAM. The guest runs QEMU's
`EPYC-Genoa` CPU model rather than `-cpu host`, so the published measurement does
not depend on the host's exact CPU stepping. The host needs QEMU 8.1 or newer.
Linux is compiled from hash-pinned source using the StageX toolchain. SNP and
virtio network drivers are built in; loadable modules and core dumps are disabled.
The archive includes the kernel configuration. BusyBox, Caddy and the CA bundle
come from pinned StageX images. SNP firmware remains a pinned Ubuntu OVMF build.
The initramfs includes only the runtime files.

`Dockerfile.guest` builds the whole guest with StageX: kernel, firmware,
initramfs (the kernel's `gen_init_cpio`), host unit and launch measurement
(`snpguest`). Launch settings are build arguments; measurement and launch share
them. To build locally from release binaries:

    docker buildx build -f Dockerfile.guest --platform linux/amd64 --target guest \
      --build-context resolvers=<directory with zns-resolver-*-x86_64-linux-musl> \
      --output type=local,dest=guest-release .

Add `--build-arg ACME_DIRECTORY=https://acme-staging-v02.api.letsencrypt.org/directory`
for an issuance-testing image with untrusted staging certificates.

The guest has no disk. A VM has no private storage: any attached disk is a host
file the host can read and rewrite. All state therefore stays in encrypted guest
RAM. TLS/ACME keys and logs live under `/run`. The registry databases live under
`/data`, and each boot rebuilds them by scanning the chain from the mint
birthday. No host directory is shared into the guest. Restarting the guest loses
its certificates and requires successful issuance before HTTPS works again.
Resolvers report `synced: false` until each rescan reaches the chain tip. Frequent restarts may hit CA rate limits, so the unit deliberately
does not restart itself automatically. Use manual workflow runs with `staging`
enabled for issuance testing; staging certificates are not publicly trusted.

## Certificate validation during migration

The guest uses HTTP-01 on port 80. Before directing public traffic to this guest,
forward host TCP ports 80 and 443 into it, permit them in the host firewall, and
ensure every IP returned by DNS can answer its HTTP-01 challenges. The host must
not terminate TLS or receive the guest's private keys.

## Host installation

Download a specific release and verify its outer `SHA256SUMS`, then extract the
guest archive under `/opt`. Verify its inner `SHA256SUMS` in
`/opt/zns-resolver-guest`. Keep the boot files and launch configuration together.

Install the included systemd unit in `/etc/systemd/system`, reload systemd, and
start it. The unit verifies the packaged checksums on each start. It binds host
IPv4 ports 80/443 and host-loopback port 13080 for `/status.json`; it does not
publish either resolver's plaintext RPC port. Enable it explicitly if it should
start at host boot. Ports 80, 443 and 13080 must be available; stop the existing
resolver prototype before replacing it. Unrelated guests can keep running.
Remove stale AAAA records unless equivalent IPv6 forwarding is set
up. Complete HTTPS validation on the new IP before adding it to DNS.

Public ACME issuance/renewal and an SNP boot of each new release must be checked
on the real host before traffic cutover. This PR does not change DNS, nginx, or
the running prototype. Ordinary HTTPS clients do not verify SNP evidence and
cannot detect a domain operator selectively impersonating the guest.
