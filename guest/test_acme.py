"""Run only inside the network-disabled ACME test container, never on the host."""
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

from test_gateway import main as check_gateway, stop


class ChallengeRelay(BaseHTTPRequestHandler):
    requests = 0

    def log_message(self, *_):
        pass

    def do_GET(self):
        if not self.path.startswith('/.well-known/acme-challenge/'):
            self.send_error(404)
            return
        type(self).requests += 1
        upstream = http.client.HTTPConnection('127.0.0.1', 18080, timeout=5)
        try:
            upstream.request('GET', self.path, headers={'Host': self.headers['Host']})
            response = upstream.getresponse()
            body = response.read()
            self.send_response(response.status)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        finally:
            upstream.close()


with tempfile.TemporaryDirectory(prefix='zns-acme-') as temporary:
    work = Path(temporary)
    # This is the disposable container's hosts file, not the developer's DNS.
    with Path('/etc/hosts').open('a') as hosts:
        hosts.write('\n127.0.0.2 main.zcashnames.com light.zcash.me\n')
    config = {'pebble': {
        'listenAddress': '127.0.0.1:14000', 'managementListenAddress': '127.0.0.1:15000',
        'certificate': '/pebble-certs/localhost/cert.pem', 'privateKey': '/pebble-certs/localhost/key.pem',
        'httpPort': 18080, 'tlsPort': 18443,
        'profiles': {'default': {'description': 'local renewal test', 'validityPeriod': 15}},
    }}
    config_path = work / 'pebble.json'
    config_path.write_text(json.dumps(config))
    relay = ThreadingHTTPServer(('127.0.0.2', 18080), ChallengeRelay)
    threading.Thread(target=relay.serve_forever, daemon=True).start()
    with (work / 'pebble.log').open('wb') as log:
        ca = subprocess.Popen(['pebble', '-config', str(config_path)], stdout=log, stderr=log,
                              env=dict(os.environ, PEBBLE_VA_NOSLEEP='1', PEBBLE_AUTHZREUSE='0',
                                       PEBBLE_WFE_NONCEREJECT='0'))
        try:
            context = ssl.create_default_context(cafile='/pebble-certs/pebble.minica.pem')
            for attempt in range(100):
                try:
                    with urllib.request.urlopen('https://localhost:15000/roots/0', context=context, timeout=2) as response:
                        (work / 'root.pem').write_bytes(response.read())
                    break
                except OSError:
                    time.sleep(0.1)
            else:
                raise AssertionError('Local ACME server did not start')
            sys.argv = ['test_gateway.py', '--caddy', '/userspace/usr/bin/caddy', '--binaries', '/binaries',
                        '--acme-directory', 'https://localhost:14000/dir',
                        '--acme-root', '/pebble-certs/pebble.minica.pem',
                        '--certificate-root', str(work / 'root.pem')]
            check_gateway()
            assert ChallengeRelay.requests > 0, 'HTTP-01 relay was not exercised'
            print('PASS: ACME HTTP-01 through old-host relay, automatic renewal, and reissuance after restart.')
        except Exception:
            log.flush()
            print((work / 'pebble.log').read_text(errors='replace'), flush=True)
            raise
        finally:
            stop(ca)
            relay.shutdown()
