"""Assemble the measured guest; invoked by the release workflow on Linux."""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import shutil
import stat
import struct
import subprocess
import tarfile
import tomllib
import urllib.request

HERE = Path(__file__).resolve().parent


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def compress(data):
    # GzipFile fixes the OS byte too; gzip.compress(mtime=0) differs across
    # Python versions/platforms. Omit filenames and wall-clock timestamps.
    stream = io.BytesIO()
    with gzip.GzipFile(fileobj=stream, mode='wb', filename='', mtime=0) as output:
        output.write(data)
    return stream.getvalue()


def fetch(url, digest, cache):
    path = cache / digest
    if not path.exists() or sha256(path.read_bytes()) != digest:
        with urllib.request.urlopen(url, timeout=120) as response:
            data = response.read()
        if sha256(data) != digest:
            raise ValueError(f"Checksum mismatch: {url}")
        path.write_bytes(data)
    return path.read_bytes()


def static_amd64(data, name):
    if data[:6] != b'\x7fELF\x02\x01' or struct.unpack_from('<H', data, 18)[0] != 62:
        raise ValueError(f'Not a Linux x86-64 ELF: {name}')
    phoff = struct.unpack_from('<Q', data, 32)[0]
    phsize, phnum = struct.unpack_from('<HH', data, 54)
    for index in range(phnum):
        if struct.unpack_from('<I', data, phoff + index * phsize)[0] == 3:
            raise ValueError(f'Unexpected ELF interpreter: {name}')


def initramfs(files):
    """newc archive with fixed ownership, ordering, timestamps and permissions."""
    entries = {'.': (stat.S_IFDIR | 0o755, b'')}
    for name, (mode, data) in files.items():
        for parent in Path(name).parents:
            if parent.as_posix() != '.':
                entries[parent.as_posix()] = (stat.S_IFDIR | 0o755, b'')
        entries[name] = (mode, data)
    for name in ('proc', 'sys', 'dev', 'run', 'tmp', 'data'):
        entries[name] = (stat.S_IFDIR | 0o755, b'')
    stream = io.BytesIO()
    for inode, (name, (mode, data)) in enumerate([*sorted(entries.items()), ('TRAILER!!!', (0, b''))], 1):
        encoded = name.encode() + b'\0'
        fields = [inode, mode, 0, 0, 2 if stat.S_ISDIR(mode) else 1, 0, len(data), 0, 0, 0, 0, len(encoded), 0]
        stream.write(b'070701' + ''.join(f'{field:08x}' for field in fields).encode())
        stream.write(encoded)
        stream.write(b'\0' * (-stream.tell() % 4))
        stream.write(data)
        stream.write(b'\0' * (-stream.tell() % 4))
    return compress(stream.getvalue())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--userspace', type=Path, required=True)
    parser.add_argument('--binaries', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cache', type=Path, required=True)
    parser.add_argument('--measure-tool', required=True)
    parser.add_argument('--source-commit', required=True)
    parser.add_argument('--staging', action='store_true')
    args = parser.parse_args()
    pins = tomllib.loads((HERE / 'versions.toml').read_text())
    args.cache.mkdir(parents=True, exist_ok=True)
    args.output.mkdir(parents=True, exist_ok=True)
    files = {}
    for name in ('usr/bin/busybox', 'usr/bin/caddy', 'etc/ssl/certs/ca-certificates.crt'):
        data = (args.userspace / name).read_bytes()
        if name.startswith('usr/bin/'):
            static_amd64(data, name)
        files[name] = (stat.S_IFREG | (0o755 if name.startswith('usr/bin/') else 0o644), data)
    for network in ('mainnet', 'testnet'):
        name = f'zns-resolver-{network}-x86_64-linux-musl'
        data = (args.binaries / name).read_bytes()
        static_amd64(data, name)
        files[f'usr/bin/{name}'] = (stat.S_IFREG | 0o755, data)
    files['bin/sh'] = (stat.S_IFLNK | 0o777, b'../usr/bin/busybox')
    files['init'] = (stat.S_IFREG | 0o755, (HERE / 'init').read_bytes())
    config = (HERE / 'Caddyfile').read_text()
    if args.staging:
        config = config.replace('https://acme-v02.api.letsencrypt.org/directory',
                                'https://acme-staging-v02.api.letsencrypt.org/directory')
    files['etc/caddy/Caddyfile'] = (stat.S_IFREG | 0o644, config.encode())
    for name in ('vmlinuz', 'kernel.config', 'OVMF.amdsev.fd'):
        data = (args.userspace / 'boot' / name).read_bytes()
        if name == 'OVMF.amdsev.fd' and sha256(data) != pins['firmware_sha256']:
            raise ValueError('Firmware checksum mismatch')
        (args.output / name).write_bytes(data)
    (args.output / 'resolver-initrd.img').write_bytes(initramfs(files))
    service = (HERE / 'zns-resolver-guest.service.in').read_text()
    for key, value in {'VCPUS': pins['vcpus'], 'MEMORY': pins['memory'],
                       'POLICY': pins['guest_policy'], 'CMDLINE': pins['cmdline']}.items():
        service = service.replace(f'@{key}@', str(value))
    (args.output / 'zns-resolver-guest.service').write_text(service)
    command = [args.measure_tool, '--mode', 'snp', '--vmm-type', 'QEMU',
               '--vcpus', str(pins['vcpus']), '--vcpu-family', str(pins['cpu_family']),
               '--vcpu-model', str(pins['cpu_model']), '--vcpu-stepping', str(pins['cpu_stepping']),
               '--guest-features', pins['guest_features'], '--ovmf', str(args.output / 'OVMF.amdsev.fd'),
               '--kernel', str(args.output / 'vmlinuz'), '--initrd', str(args.output / 'resolver-initrd.img'),
               '--append', pins['cmdline']]
    measurement = subprocess.run(command, check=True, capture_output=True, text=True).stdout.strip()
    if len(measurement) != 96 or any(c not in '0123456789abcdef' for c in measurement):
        raise ValueError('Invalid SNP measurement output')
    (args.output / 'expected-snp-measurement.txt').write_text(measurement + '\n')
    manifest = dict(pins, source_commit=args.source_commit, staging=args.staging,
                    expected_snp_measurement=measurement, attestation_verifier=False,
                    stagex_images=[line for line in (HERE.parent / 'Dockerfile.guest').read_text().splitlines()
                                   if line.startswith('FROM stagex/')],
                    kernel_source=[line for line in (HERE.parent / 'Dockerfile.guest').read_text().splitlines()
                                   if line.startswith('ADD --checksum=')],
                    measured_files={name: sha256(data) for name, (_, data) in files.items()})
    (args.output / 'guest-manifest.json').write_text(json.dumps(manifest, sort_keys=True, indent=2) + '\n')
    names = ['vmlinuz', 'kernel.config', 'OVMF.amdsev.fd', 'resolver-initrd.img', 'zns-resolver-guest.service',
             'expected-snp-measurement.txt', 'guest-manifest.json']
    checksums = ''.join(f'{sha256((args.output / name).read_bytes())}  {name}\n' for name in names)
    (args.output / 'SHA256SUMS').write_text(checksums)
    with io.BytesIO() as stream:
        with tarfile.open(fileobj=stream, mode='w', format=tarfile.USTAR_FORMAT) as archive:
            for name in sorted([*names, 'SHA256SUMS']):
                data = (args.output / name).read_bytes()
                item = tarfile.TarInfo('zns-resolver-guest/' + name)
                item.size = len(data)
                item.mode = 0o644
                archive.addfile(item, io.BytesIO(data))
        (args.output / 'zns-resolver-guest-x86_64.tar.gz').write_bytes(compress(stream.getvalue()))
    print(f'Guest SNP measurement: {measurement}')


if __name__ == '__main__':
    main()
