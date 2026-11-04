# SEV-SNP guest for both resolvers, built entirely from pinned StageX images:
#   docker buildx build -f .github/guest/Dockerfile --platform linux/amd64 --target guest \
#     --build-context resolvers=<dir with zns-resolver-*-x86_64-linux-musl> \
#     --output type=local,dest=guest-release .
FROM stagex/user-elfutils@sha256:b5fc481f468dbab1d39bc3a2f2b256153cdcaf33e135abf41a2c5b5bea539151 AS elfutils
FROM stagex/core-openssl@sha256:f97827b11a98c010113e0d6acc464c77cbf6ea47a67867cfd1baff093bc5399a AS openssl
FROM stagex/core-ncurses@sha256:a5875ef06637624ead593724c43ab27be5fc7dbec5d292022bfe234896f584c1 AS ncurses
FROM stagex/core-libzstd@sha256:84c78c0000a4134df8a45661ab7e8ecb91dead720ff6b46c315380fb33cd0607 AS zstd
FROM stagex/core-busybox@sha256:a094d1b5ad06943713e75849248d7f89546d15a3e74f42ec5b8f02375fd3706d AS busybox
FROM stagex/user-caddy@sha256:f2ee0153fc2cb7c8fa017e982e1c48f9660bd76a1b650c8b4c710a1168bfc900 AS caddy
FROM stagex/core-ca-certificates@sha256:2e5d0da04413dfbc739f3009fe6616e90e733c463f8495ce6997b02c0832735c AS certificates
FROM stagex/pallet-python@sha256:9463c00f462325a7b0a46a5460b241c09a41f62e5968b1b3dea3796451caea55 AS python
FROM stagex/pallet-gcc-gnu-gnu@sha256:7c3726b921c4478d6eb95ab73edca3e185bc710e61005f4cf661267816fda0da AS tools
COPY --from=elfutils / /
COPY --from=openssl / /
COPY --from=ncurses / /

FROM tools AS kernel
ADD --checksum=sha256:f9fef3d14c0df53819026f4be74459835c2a0b0dcbf5b5bbd9ea19f0829402b3 https://cdn.kernel.org/pub/linux/kernel/v7.x/linux-7.2.tar.xz /tmp/linux.tar.xz
ENV KBUILD_BUILD_USER=stagex KBUILD_BUILD_HOST=stagex KBUILD_BUILD_TIMESTAMP="Thu Jan 1 00:00:00 UTC 1970" KBUILD_BUILD_VERSION=1 SOURCE_DATE_EPOCH=0
WORKDIR /build
RUN --network=none <<'EOF'
set -eu
ln -s /bin/sh /bin/bash
tar -xJf /tmp/linux.tar.xz --strip-components=1
make ARCH=x86_64 tinyconfig
required="64BIT EXPERT PRINTK BUG MULTIUSER FUTEX EPOLL SIGNALFD TIMERFD EVENTFD FILE_LOCKING POSIX_TIMERS
 ADVISE_SYSCALLS MEMBARRIER BINFMT_ELF BINFMT_SCRIPT PCI PCI_MSI ACPI CPU_SUP_AMD SMP HYPERVISOR_GUEST PARAVIRT
 KVM_GUEST AMD_MEM_ENCRYPT VIRT_DRIVERS SEV_GUEST EFI EFI_STUB RELOCATABLE RANDOMIZE_BASE CPU_MITIGATIONS
 STACKPROTECTOR STACKPROTECTOR_STRONG STRICT_KERNEL_RWX BLK_DEV_INITRD DEVTMPFS PROC_FS SYSFS SHMEM TMPFS
 VIRTIO_MENU VIRTIO VIRTIO_PCI NET NETDEVICES NET_CORE INET UNIX PACKET VIRTIO_NET TTY SERIAL_8250
 SERIAL_8250_CONSOLE"
for option in $required; do scripts/config --enable "$option"; done
scripts/config --disable UNWINDER_ORC --disable MODULES --disable COREDUMP --disable DEBUG_INFO \
 --disable LOCALVERSION_AUTO --set-str LOCALVERSION ''
make ARCH=x86_64 olddefconfig
for option in $required TSM_REPORTS; do grep -qx "CONFIG_$option=y" .config || { echo "Missing: $option"; exit 1; }; done
make ARCH=x86_64 -j"$(nproc)" bzImage
gcc -O2 -o /usr/bin/gen_init_cpio usr/gen_init_cpio.c
EOF

# SNP firmware: OVMF from a pinned Ubuntu package.
FROM tools AS firmware
COPY --from=zstd /usr/bin/zstd /usr/bin/zstd
COPY --from=zstd /usr/lib/libzstd.so.1 /usr/lib/libzstd.so.1
ADD --checksum=sha256:c38af8c8836ff73108ee449df9972381e9fcf47ab5e124ade836865f143691d6 https://archive.ubuntu.com/ubuntu/pool/main/e/edk2-hwe/ovmf-amdsev-hwe_2025.11-3ubuntu8.2_all.deb /tmp/ovmf.deb
RUN --network=none ar p /tmp/ovmf.deb data.tar.zst | zstd -dc | tar -xOf - ./usr/share/ovmf/OVMF.amdsev.fd > /OVMF.amdsev.fd

FROM kernel AS initramfs
COPY --from=busybox /usr/bin/busybox /r/
COPY --from=caddy /usr/bin/caddy /r/
COPY --from=certificates /etc/ssl/certs/ca-certificates.crt /r/
COPY --from=resolvers zns-resolver-mainnet-x86_64-linux-musl zns-resolver-testnet-x86_64-linux-musl /r/
COPY .github/guest/init .github/guest/Caddyfile /r/
RUN --network=none <<'EOF'
set -eu
{
 for dir in bin dev etc etc/caddy etc/ssl etc/ssl/certs proc sys run tmp data data/mainnet data/testnet usr usr/bin; do
  echo "dir /$dir 755 0 0"
 done
 echo "nod /dev/console 600 0 0 c 5 1"
 echo "slink /bin/sh ../usr/bin/busybox 777 0 0"
 echo "file /init /r/init 755 0 0"
 for binary in busybox caddy zns-resolver-mainnet-x86_64-linux-musl zns-resolver-testnet-x86_64-linux-musl; do
  echo "file /usr/bin/$binary /r/$binary 755 0 0"
 done
 echo "file /etc/caddy/Caddyfile /r/Caddyfile 644 0 0"
 echo "file /etc/ssl/certs/ca-certificates.crt /r/ca-certificates.crt 644 0 0"
} > /r/list
gen_init_cpio -t 0 /r/list > /resolver-initrd.img
EOF

# Launch measurement and host unit share these values. The unit runs -cpu host,
# so the measurement uses the host CPU signature (AMD EPYC 8024P), as zns-deployment does.
FROM python AS release
ARG VCPUS=2 MEMORY=4G POLICY=0x30000 CPU_FAMILY=25 CPU_MODEL=160 CPU_STEPPING=2 CMDLINE="console=ttyS0 rdinit=/init"
# The measurement CLI needs only the standard library, so it runs from the wheel.
ADD --checksum=sha256:feda1aebe69dcbf38e9c636ed1d082bb7dc741fc6969f197d39faddb72b2762d https://files.pythonhosted.org/packages/12/96/5b673738d1f973700369d9e80c1c32937d230acef721141e532891441213/sev_snp_measure-0.0.13-py3-none-any.whl /tmp/
COPY --from=kernel /build/arch/x86/boot/bzImage /out/vmlinuz
COPY --from=kernel /build/.config /out/kernel.config
COPY --from=firmware /OVMF.amdsev.fd /out/
COPY --from=initramfs /resolver-initrd.img /out/
COPY .github/guest/zns-resolver-guest.service.in /tmp/
WORKDIR /out
RUN --network=none <<'EOF'
set -eu
echo 'a90ff0c9335690c8a82c89c71c71f082f7c8ed41bb6a4f017fbc90498092949b  OVMF.amdsev.fd' | sha256sum -c -
sed -e "s#@VCPUS@#$VCPUS#" -e "s#@MEMORY@#$MEMORY#" -e "s#@POLICY@#$POLICY#" \
 -e "s#@CMDLINE@#$CMDLINE#" /tmp/zns-resolver-guest.service.in > zns-resolver-guest.service
PYTHONPATH=/tmp/sev_snp_measure-0.0.13-py3-none-any.whl python3 -m sevsnpmeasure.cli --mode snp --vmm-type QEMU \
 --vcpus "$VCPUS" --vcpu-family "$CPU_FAMILY" --vcpu-model "$CPU_MODEL" --vcpu-stepping "$CPU_STEPPING" \
 --guest-features 0x1 --ovmf OVMF.amdsev.fd --kernel vmlinuz --initrd resolver-initrd.img --append "$CMDLINE" \
 > expected-snp-measurement.txt
grep -qxE '[0-9a-f]{96}' expected-snp-measurement.txt
sha256sum vmlinuz kernel.config OVMF.amdsev.fd resolver-initrd.img zns-resolver-guest.service \
 expected-snp-measurement.txt > SHA256SUMS
EOF

FROM scratch AS guest
COPY --from=release /out/ /
