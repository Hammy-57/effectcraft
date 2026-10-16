import importlib.util
from pathlib import Path
import hashlib
import json
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('check_owner_icons', Path(__file__).with_name('check-owner-icons.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class OwnerIcons(unittest.TestCase):
    def test_missing_install_uses_fallback_but_approved_artwork_cannot_silently_change(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            self.assertEqual(module.check(root), [])
            atlas = root / 'native-icons.alpha'
            atlas.write_bytes(b'synthetic icon data')
            (root / 'approved-icons.json').write_text(json.dumps({atlas.name: hashlib.sha256(atlas.read_bytes()).hexdigest()}))
            self.assertEqual(module.check(root), [])
            atlas.write_bytes(b'wrong icons')
            self.assertEqual(module.check(root), [atlas.name])
            atlas.unlink()
            self.assertEqual(module.check(root), [atlas.name])

    def test_manifest_cannot_escape_asset_folder(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / 'approved-icons.json').write_text('{"../outside": "hash"}')
            with self.assertRaises(ValueError):
                module.check(root)


if __name__ == '__main__':
    unittest.main()
