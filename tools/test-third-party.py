#!/usr/bin/env python3
"""Check attribution preservation and fail-closed notice collection."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('notices', Path(__file__).with_name('third-party-notices.py'))
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


class NoticeTests(unittest.TestCase):
    def test_shared_license_preserves_each_attribution_and_extra_notices(self):
        with tempfile.TemporaryDirectory() as temp:
            packages = []
            for name in ['first', 'second']:
                root = Path(temp) / name
                root.mkdir()
                (root / 'LICENSE').write_text('Shared license text\n')
                (root / 'NOTICE').write_text(f'Copyright {name}\n')
                packages.append(dict(name=name, version='1.0.0', manifest_path=str(root / 'Cargo.toml'), license='MIT', source='registry'))
            text = notices.render(packages)
            self.assertEqual(text.count('Shared license text'), 1)
            for name in ['first', 'second']:
                self.assertIn(f'{name} 1.0.0: LICENSE', text)
                self.assertIn(f'Copyright {name}', text)
            self.assertNotIn(temp, text)

    def test_missing_and_empty_license_texts_fail(self):
        with tempfile.TemporaryDirectory() as temp:
            package = dict(name='missing', version='1.0.0', manifest_path=str(Path(temp) / 'Cargo.toml'), license='MIT', source='registry')
            with self.assertRaisesRegex(ValueError, 'Missing license'):
                notices.render([package])
            (Path(temp) / 'LICENSE').write_text('')
            with self.assertRaisesRegex(ValueError, 'Empty license'):
                notices.render([package])


if __name__ == '__main__':
    unittest.main()
