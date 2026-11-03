# HTTPS resolver guest

The Release workflow publishes `zns-resolver-guest-x86_64.tar.gz` alongside the
static binaries. This archive includes the kernel, SNP firmware, initramfs,
systemd unit, checksums, launch inputs, and expected SNP launch measurement.
Resolver logic and the JSON-RPC API are unchanged. There is no attestation
endpoint or external verifier in this version; status output is not attestation.

Both resolvers share one guest with 2 vCPUs and 4 GiB RAM. The published
measurement targets `znstwo`'s CPU signature: family 25, model 160, stepping 2.
Kernel and firmware are pinned Ubuntu packages. BusyBox, Caddy and the CA bundle
come from pinned StageX images. The initramfs includes only the runtime files.

Caddy generates fresh TLS/ACME keys each boot. All gateway state and process
logs stay in guest RAM under `/run`; the public registry databases alone are
stored on `/data`. No host directory is shared into the guest. Restarting the
guest loses its certificates and requires successful issuance before HTTPS
works again. Frequent restarts may hit CA rate limits, so the unit deliberately
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
Create a new sparse raw ext4 disk at `/opt/zns-resolver-guest/state.img` before
the first launch (8 GiB is the initial allocation); never reformat an existing
disk. It will hold separate mainnet and testnet database directories.

Install the included systemd unit in `/etc/systemd/system`, reload systemd, and
start it. The unit verifies the packaged checksums on each start. It binds host
IPv4 ports 80/443 and host-loopback port 13080 for `/status.json`; it does not
publish either resolver's plaintext RPC port. Enable it explicitly if it should
start at host boot. Ports 80, 443 and 13080 must be available; stop the existing
resolver prototype before replacing it. Unrelated guests can keep running.
Remove stale AAAA records unless equivalent IPv6 forwarding is set
up. Complete HTTPS validation on the new IP before adding it to DNS.

The workflow checks HTTPS with a local test CA, routing, redirects, CORS, database
persistence, fresh certificates between boots, and repeated image assembly. A
network-disabled container also exercises ACME HTTP-01 through a simulated
old-host challenge relay, renewal, and reissuance against Let's Encrypt's Pebble
test server. Test CA keys and certificates are excluded from the guest image.
Public ACME issuance/renewal and an SNP boot of each new release must be checked
on the real host before traffic cutover. This PR does not change DNS, nginx, or
the running prototype. Ordinary HTTPS clients do not verify SNP evidence and
cannot detect a domain operator selectively impersonating the guest.
