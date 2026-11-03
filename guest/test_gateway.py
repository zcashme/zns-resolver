"""Offline integration check of the shipped gateway routes and resolver binaries.

Uses Caddy's test CA rather than issuing public certificates. Run in the local
Docker test environment or on the native Linux x86-64 CI runner.
"""
import argparse
import http.client
import json
import os
from pathlib import Path
import socket
import sqlite3
import ssl
import subprocess
import tempfile
import time

HERE = Path(__file__).resolve().parent


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def request(host, path, method='POST', payload=None, context=None):
    if context:
        connection = http.client.HTTPSConnection(host, 18443, timeout=5, context=context)
        # Keep certificate hostname verification while dialing local test ports.
        connection._create_connection = lambda *args, **kwargs: socket.create_connection(('127.0.0.1', 18443), 5)
    else:
        connection = http.client.HTTPConnection('127.0.0.1', 18080, timeout=5)
    headers = {'Host': host, 'Content-Type': 'application/json'}
    body = json.dumps(payload) if payload is not None else None
    try:
        connection.request(method, path, body=body, headers=headers)
        response = connection.getresponse()
        return response.status, dict(response.getheaders()), response.read()
    finally:
        connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--caddy', type=Path, required=True)
    parser.add_argument('--binaries', type=Path, required=True)
    parser.add_argument('--acme-directory')
    parser.add_argument('--acme-root', type=Path)
    parser.add_argument('--certificate-root', type=Path)
    args = parser.parse_args()
    caddy = str(args.caddy.resolve())
    binaries = args.binaries.resolve()
    adapted = subprocess.run([caddy, 'adapt', '--config', str(HERE / 'Caddyfile'),
                              '--adapter', 'caddyfile'], check=True, capture_output=True, text=True)
    base = json.loads(adapted.stdout)
    # Preserve the shipped HTTP routing; replace only issuer, storage and bind ports.
    base['apps']['http']['http_port'] = 18080
    base['apps']['http']['https_port'] = 18443
    for server in base['apps']['http']['servers'].values():
        server['listen'] = ['127.0.0.1:18443' if address == ':443' else '127.0.0.1:18080'
                            for address in server['listen']]
    for policy in base['apps']['tls']['automation']['policies']:
        assert policy['issuers'][0]['module'] == 'acme'
        assert policy['issuers'][0]['challenges']['tls-alpn']['disabled'] is True
        if args.acme_directory:
            policy['issuers'][0].update(ca=args.acme_directory, test_ca=args.acme_directory,
                                       trusted_roots_pem_files=[str(args.acme_root)])
        else:
            policy['issuers'] = [{'module': 'internal'}]
    if args.acme_directory:
        base['apps']['tls']['automation']['renew_interval'] = 1_000_000_000
    # Never modify the test machine's system trust store.
    base['apps']['pki'] = {'certificate_authorities': {'local': {'install_trust': False}}}
    routes = [('main.zcashnames.com', '/', 'mainnet'),
              ('main.zcashnames.com', '/another-path', 'mainnet'),
              ('light.zcash.me', '/zns-mainnet', 'mainnet'),
              ('light.zcash.me', '/zns-mainnet-test', 'mainnet'),
              ('light.zcash.me', '/zns-mainnet?probe=1', 'mainnet'),
              ('light.zcash.me', '/zns-testnet', 'testnet')]
    leaves = []
    with tempfile.TemporaryDirectory(prefix='zns-gateway-') as temporary:
        work = Path(temporary)
        for boot in (1, 2):
            runtime = work / f'run-{boot}'
            runtime.mkdir()
            env = dict(os.environ, HOME=str(runtime), XDG_DATA_HOME=str(runtime / 'data'),
                       XDG_CONFIG_HOME=str(runtime / 'config'))
            config = json.loads(json.dumps(base))
            config['storage'] = {'module': 'file_system', 'root': str(runtime / 'storage')}
            config_path = runtime / 'caddy.json'
            config_path.write_text(json.dumps(config))
            processes = []
            with (runtime / 'process.log').open('wb') as log:
                try:
                    for network in ('mainnet', 'testnet'):
                        directory = work / network
                        directory.mkdir(exist_ok=True)
                        processes.append(subprocess.Popen(
                            [str(binaries / f'zns-resolver-{network}-x86_64-linux-musl')],
                            cwd=directory, env=env, stdout=log, stderr=log))
                    processes.append(subprocess.Popen([caddy, 'run', '--config', str(config_path)],
                                                      env=env, stdout=log, stderr=log))
                    root = args.certificate_root or runtime / 'storage/pki/authorities/local/root.crt'
                    for attempt in range(300):
                        if any(process.poll() is not None for process in processes):
                            raise AssertionError('Gateway or resolver exited during startup')
                        try:
                            context = ssl.create_default_context(cafile=str(root))
                            for host, path in [('main.zcashnames.com', '/'), ('light.zcash.me', '/zns-testnet')]:
                                code, _, body = request(host, path, payload={
                                    'jsonrpc': '2.0', 'id': 1, 'method': 'status', 'params': []}, context=context)
                                assert code == 200 and 'result' in json.loads(body)
                            break
                        except (OSError, AssertionError, json.JSONDecodeError):
                            pass
                        time.sleep(0.1)
                    else:
                        raise AssertionError('Gateway did not become ready')
                    for host, path, network in routes:
                        code, headers, body = request(host, path, payload={
                            'jsonrpc': '2.0', 'id': 1, 'method': 'status', 'params': []}, context=context)
                        assert code == 200, (host, path, code)
                        result = json.loads(body)['result']
                        assert result['viewing_key'].startswith('uview1' if network == 'mainnet' else 'uviewtest1')
                        assert headers.get('Access-Control-Allow-Origin') == '*', (host, path, code, headers)
                        code, headers, _ = request(host, path, method='OPTIONS', context=context)
                        assert code == 204 and headers.get('Access-Control-Allow-Origin') == '*', (host, path, code, headers)
                        code, headers, _ = request(host, path, method='GET')
                        assert code == 301 and headers.get('Location') == f'https://{host}{path}', (host, path, code, headers)
                    for path in ('/unknown', '/zns-mainnet/', '/zns-testnet/extra'):
                        code, _, _ = request('light.zcash.me', path, context=context)
                        assert code == 404, (path, code)
                    canary = 'privacycanary123'
                    code, _, body = request('light.zcash.me', '/zns-mainnet', payload={
                        'jsonrpc': '2.0', 'id': 2, 'method': 'resolve',
                        'params': {'query': canary}}, context=context)
                    assert code == 200 and json.loads(body)['result'] is None
                    with socket.create_connection(('127.0.0.1', 18443), timeout=5) as raw:
                        with context.wrap_socket(raw, server_hostname='main.zcashnames.com') as tls:
                            leaves.append(tls.getpeercert(binary_form=True))
                    if args.acme_directory and boot == 1:
                        # Pebble issues 15-second test certificates. Observe renewal
                        # without restarting Caddy or injecting a replacement key.
                        for attempt in range(100):
                            time.sleep(0.25)
                            with socket.create_connection(('127.0.0.1', 18443), timeout=5) as raw:
                                with context.wrap_socket(raw, server_hostname='main.zcashnames.com') as tls:
                                    renewed = tls.getpeercert(binary_form=True)
                            if renewed != leaves[-1]:
                                break
                        else:
                            raise AssertionError('ACME certificate was not renewed')
                except Exception:
                    log.flush()
                    print((runtime / 'process.log').read_text(errors='replace'), flush=True)
                    raise
                finally:
                    for process in reversed(processes):
                        stop(process)
            assert b'privacycanary123' not in (runtime / 'process.log').read_bytes()
            for network, filename in (('mainnet', 'zns.sqlite'), ('testnet', 'zns-testnet.sqlite')):
                with sqlite3.connect(work / network / filename) as connection:
                    if boot == 1:
                        connection.execute('CREATE TABLE persistence_probe (value TEXT)')
                        connection.execute("INSERT INTO persistence_probe VALUES ('kept')")
                    else:
                        assert connection.execute('SELECT value FROM persistence_probe').fetchone() == ('kept',)
                assert not list((work / network).glob('**/*.key'))
        assert leaves[0] != leaves[1], 'A fresh boot must generate a new certificate'
    print('PASS: HTTPS hostname verification, all routes, CORS, redirects, unknown paths,')
    print('      lookup log canary, persistent databases, and fresh per-boot certificate.')


if __name__ == '__main__':
    main()
