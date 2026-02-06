#!/usr/bin/env python3
"""
Audit verification test suite for straws.

Tests specific bugs and issues found during code audit, plus basic
regression tests. Designed to run against localhost SSH.

Usage:
    ./test_audit.py [options]

Options:
    --keep-temp     Don't delete temporary directories after tests
    --verbose       Show detailed output from straws commands
    --straws PATH   Path to straws binary (default: ./target/release/straws)
    --host HOST     SSH host to test against (default: localhost)
"""

import argparse
import hashlib
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Optional

# Force unbuffered output
sys.stdout.reconfigure(line_buffering=True)


class Colors:
    GREEN = '\033[92m'
    RED = '\033[91m'
    YELLOW = '\033[93m'
    BLUE = '\033[94m'
    BOLD = '\033[1m'
    RESET = '\033[0m'


def color(text: str, c: str) -> str:
    return f"{c}{text}{Colors.RESET}" if sys.stdout.isatty() else text


def compute_md5(filepath: Path) -> str:
    hasher = hashlib.md5()
    with open(filepath, 'rb') as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b''):
            hasher.update(chunk)
    return hasher.hexdigest()


def compute_md5_remote(ssh_host: str, filepath: str) -> str:
    result = subprocess.run(
        ['ssh', ssh_host, f'md5 -q "{filepath}" 2>/dev/null || md5sum "{filepath}" | cut -d" " -f1'],
        capture_output=True, text=True
    )
    if result.returncode != 0:
        raise RuntimeError(f"Failed to compute remote MD5: {result.stderr}")
    return result.stdout.strip()


class AuditTestSuite:
    def __init__(self, straws_path: str, ssh_host: str, verbose: bool = False,
                 keep_temp: bool = False):
        self.straws = Path(straws_path).resolve()
        self.ssh_host = ssh_host
        self.verbose = verbose
        self.keep_temp = keep_temp
        self.results = []

        self.temp_base = Path(tempfile.mkdtemp(prefix='straws_audit_'))
        self.local_source = self.temp_base / 'local_source'
        self.local_dest = self.temp_base / 'local_dest'
        self.remote_base = self.temp_base / 'remote'
        self.remote_source = self.remote_base / 'source'
        self.remote_dest = self.remote_base / 'dest'

        print(f"Test directory: {self.temp_base}")

    def setup(self):
        for d in [self.local_source, self.local_dest, self.remote_source, self.remote_dest]:
            d.mkdir(parents=True, exist_ok=True)

    def run_straws(self, args: list, timeout: int = 120) -> tuple:
        cmd = [str(self.straws)] + args
        if self.verbose:
            print(f"    $ {' '.join(cmd)}")
        try:
            result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
            return result.returncode, result.stdout, result.stderr
        except subprocess.TimeoutExpired:
            return -1, '', 'Command timed out'

    def clean_dest(self):
        for d in [self.local_dest, self.remote_dest]:
            if d.exists():
                shutil.rmtree(d)
            d.mkdir(parents=True, exist_ok=True)

    def run_test(self, name: str, test_func) -> bool:
        print(f"  {color('TEST:', Colors.BOLD)} {name} ... ", end='')
        start = time.time()
        try:
            test_func()
            duration = time.time() - start
            self.results.append((name, True, duration, None))
            print(f"{color('PASS', Colors.GREEN)} ({duration:.1f}s)")
            return True
        except Exception as e:
            duration = time.time() - start
            self.results.append((name, False, duration, str(e)))
            print(f"{color('FAIL', Colors.RED)}: {e}")
            return False

    # =========================================================================
    # AUDIT ISSUE TESTS
    # =========================================================================

    def test_unicode_filenames(self):
        """Test that files with unicode/multi-byte characters transfer correctly.
        This verifies the fix for the truncate() unicode panic in display.rs."""
        self.clean_dest()

        # Create files with various unicode names
        unicode_names = [
            '日本語テスト.txt',
            'archivo_español.bin',
            'Ünïcödé.dat',
            'файл.txt',
            'emoji_🎉_test.bin',
            'mïxed_café_naïve.txt',
        ]

        created = {}
        for name in unicode_names:
            filepath = self.local_source / name
            data = os.urandom(random.randint(100, 5000))
            filepath.write_bytes(data)
            created[name] = (len(data), compute_md5(filepath))

        # Upload all unicode-named files
        for name, (size, expected_md5) in created.items():
            src = str(self.local_source / name)
            dest = f"{self.ssh_host}:{self.remote_dest}/{name}"
            rc, out, err = self.run_straws(['-t', '2', '--no-progress', src, dest])
            assert rc == 0, f"Upload of '{name}' failed (rc={rc}): {err}"

        # Download them back
        for name, (size, expected_md5) in created.items():
            src = f"{self.ssh_host}:{self.remote_dest}/{name}"
            dest_file = self.local_dest / name
            rc, out, err = self.run_straws(['-t', '2', '--no-progress', src, str(self.local_dest) + '/'])
            assert rc == 0, f"Download of '{name}' failed (rc={rc}): {err}"
            assert dest_file.exists(), f"Downloaded file missing: {name}"
            actual_md5 = compute_md5(dest_file)
            assert actual_md5 == expected_md5, f"MD5 mismatch for '{name}': {actual_md5} != {expected_md5}"

    def test_unicode_in_directory_names(self):
        """Test directories with unicode names."""
        self.clean_dest()

        dir_name = '目录_données'
        nested = self.local_source / dir_name
        nested.mkdir(parents=True, exist_ok=True)

        test_file = nested / 'test.bin'
        data = os.urandom(2048)
        test_file.write_bytes(data)
        expected_md5 = compute_md5(test_file)

        src = str(self.local_source / dir_name) + '/'
        dest = f"{self.ssh_host}:{self.remote_dest}/"
        rc, out, err = self.run_straws(['-t', '2', '--no-progress', src, dest])
        assert rc == 0, f"Upload failed: {err}"

        # Download back
        src = f"{self.ssh_host}:{self.remote_dest}/"
        rc, out, err = self.run_straws(['-t', '2', '--no-progress', src, str(self.local_dest) + '/'])
        assert rc == 0, f"Download failed: {err}"

        downloaded = self.local_dest / 'test.bin'
        assert downloaded.exists(), f"Downloaded file missing"
        actual_md5 = compute_md5(downloaded)
        assert actual_md5 == expected_md5, f"MD5 mismatch: {actual_md5} != {expected_md5}"

    def test_basic_upload_download_roundtrip(self):
        """Basic upload+download roundtrip with verification."""
        self.clean_dest()

        # Create a test file
        test_file = self.local_source / 'roundtrip.bin'
        data = os.urandom(500 * 1024)  # 500KB
        test_file.write_bytes(data)
        expected_md5 = compute_md5(test_file)

        # Upload
        rc, _, err = self.run_straws([
            '-t', '2', '--no-progress', '--verify',
            str(test_file),
            f"{self.ssh_host}:{self.remote_dest}/roundtrip.bin"
        ])
        assert rc == 0, f"Upload failed: {err}"

        # Verify remote
        remote_md5 = compute_md5_remote(self.ssh_host, str(self.remote_dest / 'roundtrip.bin'))
        assert remote_md5 == expected_md5, f"Remote MD5 mismatch: {remote_md5} != {expected_md5}"

        # Download
        rc, _, err = self.run_straws([
            '-t', '2', '--no-progress', '--verify',
            f"{self.ssh_host}:{self.remote_dest}/roundtrip.bin",
            str(self.local_dest) + '/'
        ])
        assert rc == 0, f"Download failed: {err}"

        actual_md5 = compute_md5(self.local_dest / 'roundtrip.bin')
        assert actual_md5 == expected_md5, f"Download MD5 mismatch"

    def test_chunked_transfer(self):
        """Test chunked file transfer with small chunk size."""
        self.clean_dest()

        # Create 4MB file, force 1MB chunks
        test_file = self.local_source / 'chunked.bin'
        data = os.urandom(4 * 1024 * 1024)
        test_file.write_bytes(data)
        expected_md5 = compute_md5(test_file)

        # Upload with small chunks
        rc, _, err = self.run_straws([
            '-t', '4', '--chunk-size', '1M', '--no-progress', '--verify',
            str(test_file),
            f"{self.ssh_host}:{self.remote_dest}/chunked.bin"
        ])
        assert rc == 0, f"Chunked upload failed: {err}"

        remote_md5 = compute_md5_remote(self.ssh_host, str(self.remote_dest / 'chunked.bin'))
        assert remote_md5 == expected_md5, f"Remote MD5 mismatch after chunked upload"

        # Download with small chunks
        rc, _, err = self.run_straws([
            '-t', '4', '--chunk-size', '1M', '--no-progress', '--verify',
            f"{self.ssh_host}:{self.remote_dest}/chunked.bin",
            str(self.local_dest) + '/'
        ])
        assert rc == 0, f"Chunked download failed: {err}"

        actual_md5 = compute_md5(self.local_dest / 'chunked.bin')
        assert actual_md5 == expected_md5, f"MD5 mismatch after chunked download"

    def test_skip_existing(self):
        """Test that existing files are skipped (and --force overrides)."""
        self.clean_dest()

        test_file = self.local_source / 'skiptest.bin'
        data = os.urandom(10 * 1024)
        test_file.write_bytes(data)

        # Upload once
        rc, _, err = self.run_straws([
            '-t', '2', '--no-progress',
            str(test_file),
            f"{self.ssh_host}:{self.remote_dest}/skiptest.bin"
        ])
        assert rc == 0, f"First upload failed: {err}"

        # Upload again - should skip (output mentions "already complete" or "No files")
        rc, out, err = self.run_straws([
            '-t', '2', '--no-progress',
            str(test_file),
            f"{self.ssh_host}:{self.remote_dest}/skiptest.bin"
        ])
        assert rc == 0, f"Second upload failed: {err}"
        combined = out + err
        assert 'already complete' in combined.lower() or 'no files' in combined.lower(), \
            f"Expected skip message, got: {combined}"

        # Upload with --force - should transfer
        rc, _, err = self.run_straws([
            '-t', '2', '--no-progress', '--force',
            str(test_file),
            f"{self.ssh_host}:{self.remote_dest}/skiptest.bin"
        ])
        assert rc == 0, f"Forced upload failed: {err}"

    def test_empty_file(self):
        """Test empty file transfer."""
        self.clean_dest()

        test_file = self.local_source / 'empty_test.txt'
        test_file.write_bytes(b'')

        rc, _, err = self.run_straws([
            '-t', '2', '--no-progress',
            str(test_file),
            f"{self.ssh_host}:{self.remote_dest}/empty_test.txt"
        ])
        assert rc == 0, f"Empty file upload failed: {err}"

        # Check remote file exists and is empty
        result = subprocess.run(
            ['ssh', self.ssh_host,
             f'test -f "{self.remote_dest}/empty_test.txt" && stat -f%z "{self.remote_dest}/empty_test.txt" 2>/dev/null || stat -c%s "{self.remote_dest}/empty_test.txt" 2>/dev/null'],
            capture_output=True, text=True
        )
        size = int(result.stdout.strip())
        assert size == 0, f"Empty file has size {size} on remote"

    def test_single_tunnel(self):
        """Test with t=1 (no parallelism)."""
        self.clean_dest()

        test_file = self.local_source / 'single_tunnel.bin'
        data = os.urandom(100 * 1024)
        test_file.write_bytes(data)
        expected_md5 = compute_md5(test_file)

        rc, _, err = self.run_straws([
            '-t', '1', '--no-progress', '--verify',
            str(test_file),
            f"{self.ssh_host}:{self.remote_dest}/single_tunnel.bin"
        ])
        assert rc == 0, f"Single tunnel upload failed: {err}"

        remote_md5 = compute_md5_remote(self.ssh_host, str(self.remote_dest / 'single_tunnel.bin'))
        assert remote_md5 == expected_md5

    def test_directory_with_nested_structure(self):
        """Test deep directory tree transfer."""
        self.clean_dest()

        base = self.local_source / 'deep_tree'
        if base.exists():
            shutil.rmtree(base)
        paths = [
            'a/b/c/d.txt',
            'a/b/e.bin',
            'a/f.txt',
            'g.bin',
        ]
        expected = {}
        for p in paths:
            filepath = base / p
            filepath.parent.mkdir(parents=True, exist_ok=True)
            data = os.urandom(random.randint(100, 5000))
            filepath.write_bytes(data)
            expected[p] = (len(data), compute_md5(filepath))

        # Upload
        rc, _, err = self.run_straws([
            '-t', '2', '--no-progress',
            str(base) + '/',
            f"{self.ssh_host}:{self.remote_dest}/"
        ])
        assert rc == 0, f"Deep tree upload failed: {err}"

        # Verify each file on remote
        for p, (size, md5) in expected.items():
            remote_path = str(self.remote_dest / p)
            remote_md5 = compute_md5_remote(self.ssh_host, remote_path)
            assert remote_md5 == md5, f"MD5 mismatch for {p}"

    def test_verify_flag(self):
        """Test that --verify catches corruption."""
        self.clean_dest()

        test_file = self.local_source / 'verify_test.bin'
        data = os.urandom(50 * 1024)
        test_file.write_bytes(data)

        # Upload with verify (should succeed)
        rc, _, err = self.run_straws([
            '-t', '2', '--no-progress', '--verify',
            str(test_file),
            f"{self.ssh_host}:{self.remote_dest}/verify_test.bin"
        ])
        assert rc == 0, f"Verified upload failed: {err}"

    def test_nonexistent_remote_path(self):
        """Test behavior with nonexistent remote source."""
        self.clean_dest()

        rc, _, err = self.run_straws([
            '-t', '2', '--no-progress',
            f"{self.ssh_host}:/nonexistent/path/file.txt",
            str(self.local_dest) + '/'
        ])
        assert rc != 0, "Expected failure for nonexistent remote path"

    def test_multiple_files_upload(self):
        """Test uploading multiple individual files."""
        self.clean_dest()

        files = {}
        for i in range(5):
            name = f"multi_{i}.bin"
            filepath = self.local_source / name
            data = os.urandom(random.randint(1024, 10240))
            filepath.write_bytes(data)
            files[name] = compute_md5(filepath)

        sources = [str(self.local_source / name) for name in files]
        dest = f"{self.ssh_host}:{self.remote_dest}/"
        rc, _, err = self.run_straws(['-t', '4', '--no-progress'] + sources + [dest])
        assert rc == 0, f"Multi-file upload failed: {err}"

        for name, expected_md5 in files.items():
            remote_md5 = compute_md5_remote(self.ssh_host, str(self.remote_dest / name))
            assert remote_md5 == expected_md5, f"MD5 mismatch for {name}"

    # =========================================================================
    # Rust unit test verification (run cargo test)
    # =========================================================================

    def test_rust_unit_tests(self):
        """Verify all Rust unit tests pass."""
        result = subprocess.run(
            ['cargo', 'test'],
            capture_output=True, text=True,
            cwd=self.straws.parent.parent,
            timeout=120,
        )
        assert result.returncode == 0, f"cargo test failed:\n{result.stdout}\n{result.stderr}"

    # =========================================================================
    # Run all
    # =========================================================================

    def run_all_tests(self):
        print(f"\n{'=' * 60}")
        print(f"{color('STRAWS AUDIT TEST SUITE', Colors.BOLD)}")
        print(f"{'=' * 60}")
        print(f"Binary: {self.straws}")
        print(f"SSH Host: {self.ssh_host}")

        self.setup()

        print(f"\n{color('>>> RUST UNIT TESTS <<<', Colors.YELLOW)}")
        self.run_test("Rust unit tests (cargo test)", self.test_rust_unit_tests)

        print(f"\n{color('>>> BASIC TRANSFER TESTS <<<', Colors.YELLOW)}")
        self.run_test("Upload+download roundtrip with verify", self.test_basic_upload_download_roundtrip)
        self.run_test("Single tunnel (t=1)", self.test_single_tunnel)
        self.run_test("Empty file transfer", self.test_empty_file)
        self.run_test("Multiple files upload", self.test_multiple_files_upload)
        self.run_test("Deep directory tree", self.test_directory_with_nested_structure)
        self.run_test("Chunked transfer (4MB, 1M chunks)", self.test_chunked_transfer)

        print(f"\n{color('>>> SKIP / FORCE TESTS <<<', Colors.YELLOW)}")
        self.run_test("Skip existing + --force override", self.test_skip_existing)
        self.run_test("--verify flag", self.test_verify_flag)

        print(f"\n{color('>>> ERROR HANDLING TESTS <<<', Colors.YELLOW)}")
        self.run_test("Nonexistent remote path", self.test_nonexistent_remote_path)

        print(f"\n{color('>>> UNICODE / AUDIT ISSUE TESTS <<<', Colors.YELLOW)}")
        self.run_test("Unicode filenames (audit issue #1)", self.test_unicode_filenames)
        self.run_test("Unicode directory names", self.test_unicode_in_directory_names)

        self.print_summary()

    def print_summary(self):
        print(f"\n{'=' * 60}")
        print(f"{color('SUMMARY', Colors.BOLD)}")
        print(f"{'=' * 60}")

        passed = sum(1 for _, ok, _, _ in self.results if ok)
        failed = len(self.results) - passed
        total_time = sum(d for _, _, d, _ in self.results)

        print(f"Total: {len(self.results)} | {color(f'Passed: {passed}', Colors.GREEN)}", end='')
        if failed:
            print(f" | {color(f'Failed: {failed}', Colors.RED)}")
            for name, ok, _, error in self.results:
                if not ok:
                    print(f"  - {name}: {error}")
        else:
            print()

        print(f"Time: {total_time:.1f}s")

        if not self.keep_temp:
            print(f"Cleaning up {self.temp_base}...")
            shutil.rmtree(self.temp_base)
        else:
            print(f"Test dir kept: {self.temp_base}")

        return 1 if failed else 0


def main():
    parser = argparse.ArgumentParser(description='Straws audit verification tests')
    parser.add_argument('--keep-temp', action='store_true')
    parser.add_argument('--verbose', '-v', action='store_true')
    parser.add_argument('--straws', default='./target/release/straws')
    parser.add_argument('--host', default='localhost')
    args = parser.parse_args()

    straws_path = Path(args.straws)
    if not straws_path.exists():
        straws_path = Path(__file__).parent.parent / args.straws
        if not straws_path.exists():
            print(f"Error: straws not found at {args.straws}")
            sys.exit(1)

    print(f"Testing SSH to {args.host}...", end=' ')
    result = subprocess.run(['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5',
                            args.host, 'echo OK'], capture_output=True, text=True)
    if 'OK' not in result.stdout:
        print(f"FAILED: {result.stderr}")
        sys.exit(1)
    print("OK")

    suite = AuditTestSuite(str(straws_path), args.host, args.verbose, args.keep_temp)
    try:
        suite.run_all_tests()
        sys.exit(0 if all(ok for _, ok, _, _ in suite.results) else 1)
    except KeyboardInterrupt:
        print(f"\n{color('Interrupted', Colors.YELLOW)}")
        sys.exit(130)


if __name__ == '__main__':
    main()
