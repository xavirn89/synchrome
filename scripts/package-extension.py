#!/usr/bin/env python3
"""Build an allowlisted, deterministic CWS draft ZIP without changing the local identity."""
import argparse
import base64
import hashlib
import json
import re
import subprocess
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FILES = ['manifest.json', 'background.js', 'popup.js', 'popup.html', 'brand-tokens.css',
         'icons/16.png', 'icons/32.png', 'icons/48.png', 'icons/128.png']
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--output', type=Path, default=ROOT / 'dist/chrome-web-store')
args = parser.parse_args()
source = ROOT / 'apps/extension'
manifest = json.loads((source / 'manifest.json').read_text())
assert manifest['manifest_version'] == 3
assert set(manifest['permissions']) == {'storage', 'nativeMessaging', 'bookmarks', 'alarms', 'management'}
assert not manifest.get('host_permissions') and not manifest.get('content_scripts')
assert len(manifest['description']) <= 132, 'Store description exceeds 132 characters'
key_hash = hashlib.sha256(base64.b64decode(manifest['key'], validate=True)).hexdigest()[:32]
local_id = ''.join(chr(ord('a') + int(n, 16)) for n in key_hash)
pinned = re.search(r'SYNCHROME_EXTENSION_ID: &str = "([a-p]{32})"', (ROOT / 'crates/synchrome-types/src/lib.rs').read_text()).group(1)
assert local_id == pinned, 'Native allowlist and unpacked extension identity disagree'
for script in ['background.js', 'popup.js']:
    subprocess.run(['node', '--check', str(source / script)], check=True)
# CWS assigns its own signing identity. Do not claim the development key controls it.
manifest.pop('key')
args.output.mkdir(parents=True, exist_ok=True)
archive = args.output / f'Synchrome-{manifest["version"]}-ChromeWebStore.zip'
with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED) as z:
    for name in FILES:
        info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        info.external_attr = 0o100644 << 16
        data = (json.dumps(manifest, indent=2)+'\n').encode() if name == 'manifest.json' else (source / name).read_bytes()
        if name == 'icons/128.png':
            data = (ROOT / 'distribution/chrome-web-store/assets/store-icon.png').read_bytes()
        z.writestr(info, data)
with zipfile.ZipFile(archive) as z:
    assert z.testzip() is None and z.namelist() == FILES
report = {'version':manifest['version'], 'archive':archive.name, 'sha256':hashlib.sha256(archive.read_bytes()).hexdigest(),
          'bytes':archive.stat().st_size, 'files':FILES, 'unpackedExtensionId':local_id,
          'storeExtensionId':None, 'nativeCompatibilityVerified':False,
          'publicationReady':False, 'purpose':'Draft upload to obtain the official store identity'}
(args.output / 'package-validation.json').write_text(json.dumps(report, indent=2)+'\n')
print(json.dumps(report, indent=2))
