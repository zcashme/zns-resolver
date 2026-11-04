# SEV-SNP resolver guest

`Dockerfile` builds a minimal SEV-SNP guest from pinned StageX images. The
guest runs both resolvers behind Caddy, which terminates TLS inside the guest. The
build outputs the kernel, firmware, initramfs, systemd unit, expected SNP launch
measurement and `SHA256SUMS`. Releases ship it as `zns-resolver-guest-x86_64.tar.gz`.

The guest has no disk, because any disk would be host-readable storage. TLS keys
and the registry databases stay in encrypted guest RAM. Each boot rescans the chain
and requests new certificates. Restarts are therefore manual, which avoids
Let's Encrypt rate limits.

To install, extract the release to `/opt/zns-resolver-guest` and verify `SHA256SUMS`.
Copy the unit to `/etc/systemd/system`, then start it. The host needs SEV-SNP and
QEMU 8.1 or newer. Host ports 80 and 443 are forwarded to the guest. The guest
publishes no attestation endpoint yet.
