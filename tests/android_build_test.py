"""Build contract guard; actual NDK builds remain a separate evidence gate."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]


class BuildContract(unittest.TestCase):
    def test_library_only_locked_build_and_single_target_directory(self):
        script = (ROOT / 'packaging/android/build-native.sh').read_text()
        self.assertIn('--lib --locked', script)
        self.assertIn('CARGO_TARGET_DIR', script)
        self.assertIn('CC_aarch64_linux_android', script)
        self.assertIn('AR_x86_64_linux_android', script)
        apk = (ROOT / 'packaging/android/build-apk.sh').read_text()
        self.assertNotIn('cp target/', apk)
        self.assertIn('build-native.sh', apk)


if __name__ == '__main__':
    unittest.main(verbosity=2)
