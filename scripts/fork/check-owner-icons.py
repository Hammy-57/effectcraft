#!/usr/bin/env python3
"""Check the owner's approved local artwork; no artwork or hashes ship in the repo."""
import hashlib
import json
import os
from pathlib import Path
import sys


def check(root):
    manifest = root / 'approved-icons.json'
    if not manifest.exists():
        return []  # Ordinary installations use the built-in fallback icons.
    expected = json.loads(manifest.read_text())
    if not isinstance(expected, dict) or not expected:
        raise ValueError('empty or invalid approved-icons.json')
    failures = []
    for name, digest in expected.items():
        relative = Path(name)
        if relative.is_absolute() or '..' in relative.parts:
            raise ValueError('invalid icon path in approved-icons.json')
        path = root / relative
        if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            failures.append(name)
    return failures


def main():
    override = os.environ.get('EFFECTCRAFT_ICON_REFERENCE')
    default = Path.home() / 'Library/Application Support/EffectCraft/owner-assets'
    root = Path(override).parent if override else default
    try:
        if override and (default / 'approved-icons.json').exists() and not (root / 'approved-icons.json').exists():
            raise ValueError('EFFECTCRAFT_ICON_REFERENCE points to an unapproved icon folder')
        failures = check(root)
        if failures:
            raise ValueError('missing or changed: ' + ', '.join(failures))
    except (OSError, ValueError) as error:
        print(f'Owner icon verification failed: {error}. Restore the approved files in {root}, or review intentional changes before updating approved-icons.json.', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
