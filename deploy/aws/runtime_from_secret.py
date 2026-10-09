#!/usr/bin/env python3
"""Create the first production runtime.env from the instance's scoped secret.

Run on the selected host as root, without shell tracing. Captured AWS output and
secret values are never printed. Existing runtime configuration is never replaced.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import re
import stat
import subprocess
import sys
import tempfile

DESTINATION = Path('/etc/mcport/runtime.env')
SECRET_ID = 'silicon-mcport/production-runtime'
REGION = 'us-east-1'
FIXED = {
    'ACCOUNTS_URL': 'https://accounts.teamofsilicons.com',
    'MCPORT_APP_ID': 'mcport',
    'MCPORT_BIND': '127.0.0.1:4380',
    'MCPORT_DATA_DIR': '/var/lib/mcport',
    'MCPORT_PUBLIC_URL': 'https://backend.mcport.teamofsilicons.com',
    'MCPORT_WEB_URL': 'https://mcport.teamofsilicons.com',
}
OPTIONAL = {
    'MCPORT_MASTER_KEY', 'MCPORT_ACCOUNTS_WEBHOOK_SECRET', 'MCPORT_TELEMETRY_KEY',
    'POSTMARK_SERVER_TOKEN', 'MCPORT_REPORT_FROM', 'ACCOUNTS_API_URL',
}


def render_runtime(payload):
    if not isinstance(payload, dict) or set(payload) - (OPTIONAL | {'MCPORT_APP_SECRET'}):
        raise ValueError('unsupported runtime keys')
    if not payload.get('MCPORT_APP_SECRET'):
        raise ValueError('application secret required')
    for key, value in payload.items():
        if not isinstance(value, str) or not value or any(ord(c) < 32 or ord(c) == 127 for c in value):
            raise ValueError('runtime values must be nonempty single-line strings')
        if key == 'MCPORT_ACCOUNTS_WEBHOOK_SECRET' and (not value.startswith('whsec_') or len(value) < 16):
            raise ValueError('the Silicon Accounts webhook secret is the whsec_ value shown for the mcport webhook')
        if key == 'MCPORT_MASTER_KEY' and not re.fullmatch(r'[0-9a-fA-F]{64}', value):
            raise ValueError('master key requires 64 hex characters')
        if key == 'ACCOUNTS_API_URL' and not value.startswith('https://'):
            raise ValueError('the Silicon Accounts API URL must use https')
    values = {**FIXED, **payload}
    # EnvironmentFile does not perform shell variable expansion. Quote its own
    # backslash/double-quote syntax; never source this file as a shell script.
    return ''.join(key + '="' + value.replace('\\', '\\\\').replace('"', '\\"') + '"\n'
                   for key, value in sorted(values.items()))


def write_new_private(destination, content):
    temporary = None
    try:
        fd, temporary = tempfile.mkstemp(prefix='.runtime-', dir=destination.parent)
        with os.fdopen(fd, 'w') as output:
            os.fchmod(output.fileno(), 0o600)
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        # Atomic, create-only publication: unlike rename, link refuses clobbering.
        os.link(temporary, destination, follow_symlinks=False)
        directory = os.open(destination.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if temporary is not None:
            Path(temporary).unlink(missing_ok=True)


def main():
    argparse.ArgumentParser(description=__doc__).parse_args()
    if os.geteuid() != 0 or platform.system() != 'Linux' or platform.machine() not in {'aarch64', 'arm64'}:
        raise ValueError('run as root on the selected ARM64 Linux host')
    parent = DESTINATION.parent.lstat()
    if not stat.S_ISDIR(parent.st_mode) or parent.st_uid != 0 or stat.S_IMODE(parent.st_mode) != 0o700:
        raise ValueError('configuration directory must be root-owned mode0700')
    if DESTINATION.exists() or DESTINATION.is_symlink():
        raise ValueError('runtime already exists; coordinate configuration updates with deployment backup')
    response = subprocess.run(
        ['aws', '--region', REGION, '--no-cli-pager', 'secretsmanager', 'get-secret-value',
         '--secret-id', SECRET_ID, '--output', 'json'],
        check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60,
        env={**os.environ, 'AWS_PAGER': '', 'AWS_CLI_AUTO_PROMPT': 'off'},
    )
    document = json.loads(response.stdout)
    payload = json.loads(document['SecretString'])
    write_new_private(DESTINATION, render_runtime(payload))
    print(json.dumps({'runtime_file_created': str(DESTINATION), 'mode': '0600', 'services_started': False}))


if __name__ == '__main__':
    try:
        main()
    except Exception:
        # Do not render subprocess stderr, JSON fragments or exception values.
        print('Runtime setup failed. Check instance-role access, secret schema and protected file prerequisites; no secret values are logged.', file=sys.stderr)
        sys.exit(1)
