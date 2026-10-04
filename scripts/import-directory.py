#!/usr/bin/env python3
"""Import only the licensed server list, never website HTML or executable snippets.

Usage: python3 scripts/import-directory.py --revision <reviewed 40-character commit>
Review the resulting diff before shipping. Runtime startup never accesses the network.
"""
import argparse
import hashlib
import html
import json
from pathlib import Path
import re
import urllib.parse
import urllib.request

REPOSITORY = 'https://github.com/wong2/awesome-mcp-servers'
OUTPUT = Path(__file__).resolve().parents[1] / 'crates/mcport-server/catalog'
SECTIONS = {'Reference Servers', 'Official Servers', 'Community Servers'}


def download(revision, path):
    url = f'https://raw.githubusercontent.com/wong2/awesome-mcp-servers/{revision}/{path}'
    with urllib.request.urlopen(url, timeout=30) as response:
        data = response.read(2_000_001)
    if len(data) > 2_000_000:
        raise ValueError('Upstream file exceeds import limit')
    return data


def plain(value):
    value = re.sub(r'\[([^\]]+)\]\([^)]*\)', r'\1', value)
    return ' '.join(html.unescape(re.sub(r'<[^>]*>', '', value)).replace('**', '').replace('`', '').split())


def parse(text):
    entries = {}
    section = ''
    skipped = []
    for line_number, line in enumerate(text.splitlines(), 1):
        if line.startswith('## '):
            section = line[3:].strip()
        if section not in SECTIONS or not line.startswith('- '):
            continue
        match = re.match(r'-\s+(?:\*\*)?\[([^\]]+)\]\((https?://[^\s)]+)\)(?:\*\*)?\s*[-–—:]?\s*(.*)', line)
        if not match:
            skipped.append({'line': line_number, 'reason': 'Unrecognized Markdown server row'})
            continue
        name, link, description = match.groups()
        url = urllib.parse.urlsplit(link)
        if url.username or url.password or not url.hostname:
            raise ValueError('Credential-bearing or invalid source URL')
        # The stable source identity is the documentation page, not tracking/anchors.
        link = urllib.parse.urlunsplit((url.scheme, url.netloc, url.path.rstrip('/'), '', ''))
        entry = {'name': plain(name), 'description': plain(description), 'category': 'Other', 'source_url': link, 'template': None}
        if not entry['name'] or len(entry['name'].encode()) > 128 or len(entry['description'].encode()) > 4096:
            raise ValueError('Catalog text exceeds API bounds')
        entries.setdefault(link, entry)
    if not 100 <= len(entries) <= 5000:
        raise ValueError('Unexpected upstream size; review importer before updating')
    return sorted(entries.values(), key=lambda value: (value['name'].casefold(), value['source_url'])), skipped


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--revision', required=True)
    args = parser.parse_args()
    if not re.fullmatch('[0-9a-f]{40}', args.revision):
        parser.error('Use an immutable lowercase Git commit SHA')
    readme = download(args.revision, 'README.md')
    license_text = download(args.revision, 'LICENSE')
    if not license_text.startswith(b'MIT License\n'):
        raise ValueError('Upstream license changed; manual review required')
    entries, skipped = parse(readme.decode())
    templates = json.loads((OUTPUT / 'reviewed-templates.json').read_text())
    for entry in entries:
        if entry['source_url'] in templates:
            reviewed = templates[entry['source_url']]
            entry['template'] = reviewed['template']
            entry['category'] = reviewed['category']
            for field in ('name', 'description'):
                if field in reviewed:
                    entry[field] = reviewed[field]
    snapshot = {'repository': REPOSITORY, 'revision': args.revision, 'readme_sha256': hashlib.sha256(readme).hexdigest(), 'license': 'MIT', 'license_text': license_text.decode(), 'skipped_rows': skipped, 'entries': entries}
    OUTPUT.mkdir(parents=True, exist_ok=True)
    (OUTPUT / 'community.json').write_text(json.dumps(snapshot, ensure_ascii=False, indent=2) + '\n')
    (OUTPUT / 'LICENSE').write_bytes(license_text)
    print(json.dumps({'revision': args.revision, 'entries': len(entries), 'skipped_rows': len(skipped), 'reviewed_templates': sum(e['template'] is not None for e in entries)}))


if __name__ == '__main__':
    main()
