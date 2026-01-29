#!/usr/bin/env python3
"""
Comprehensive regression test suite for straws SSH file transfer tool.

Usage:
    ./test_straws.py [options]

Options:
    --keep-temp     Don't delete temporary directories after tests
    --verbose       Show detailed output from straws commands
    --straws PATH   Path to straws binary (default: ./target/release/straws)
    --host HOST     SSH host to test against (default: localhost)
    --quick         Run a quick subset of tests
"""

import argparse
import hashlib
import os
import random
import shutil
import string
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

# Force unbuffered output
sys.stdout.reconfigure(line_buffering=True)


@dataclass
class TestResult:
    name: str
    passed: bool
    duration: float
    error: Optional[str] = None


class Colors:
    GREEN = '\033[92m'
    RED = '\033[91m'
    YELLOW = '\033[93m'
    BLUE = '\033[94m'
    BOLD = '\033[1m'
    RESET = '\033[0m'


def color(text: str, c: str) -> str:
    return f"{c}{text}{Colors.RESET}" if sys.stdout.isatty() else text


def generate_random_content(size: int) -> bytes:
    """Generate random binary content."""
    if size == 0:
        return b''
    # Mix of random bytes and patterns
    content = bytearray()
    while len(content) < size:
        chunk_size = min(random.randint(1024, 32768), size - len(content))
        if random.random() < 0.7:
            content.extend(os.urandom(chunk_size))
        else:
            pattern = os.urandom(random.randint(1, 64))
            content.extend((pattern * (chunk_size // len(pattern) + 1))[:chunk_size])
    return bytes(content[:size])


def compute_md5(filepath: Path) -> str:
    """Compute MD5 hash of a file."""
    hasher = hashlib.md5()
    with open(filepath, 'rb') as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b''):
            hasher.update(chunk)
    return hasher.hexdigest()


def compute_md5_remote(ssh_host: str, filepath: str) -> str:
    """Compute MD5 hash of a remote file via SSH."""
    result = subprocess.run(
        ['ssh', ssh_host, f'md5 -q "{filepath}" 2>/dev/null || md5sum "{filepath}" | cut -d" " -f1'],
        capture_output=True, text=True
    )
    if result.returncode != 0:
        raise RuntimeError(f"Failed to compute remote MD5: {result.stderr}")
    return result.stdout.strip()


class StrawsTestSuite:
    def __init__(self, straws_path: str, ssh_host: str, verbose: bool = False,
                 keep_temp: bool = False, quick: bool = False):
        self.straws = Path(straws_path).resolve()
        self.ssh_host = ssh_host
        self.verbose = verbose
        self.keep_temp = keep_temp
        self.quick = quick
        self.results: list[TestResult] = []

        self.temp_base = Path(tempfile.mkdtemp(prefix='straws_test_'))
        self.local_source = self.temp_base / 'local_source'
        self.local_dest = self.temp_base / 'local_dest'
        self.remote_base = self.temp_base / 'remote'
        self.remote_source = self.remote_base / 'source'
        self.remote_dest = self.remote_base / 'dest'

        print(f"Test directory: {self.temp_base}")

    def setup(self):
        print(f"\n{color('Setting up test environment...', Colors.BLUE)}")

        for d in [self.local_source, self.local_dest, self.remote_source, self.remote_dest]:
            d.mkdir(parents=True, exist_ok=True)

        # Smaller test files for faster tests
        self.test_files = {
            'small_1.txt': 100,
            'small_2.bin': 1024,
            'medium_1.bin': 50 * 1024,       # 50KB
            'medium_2.bin': 200 * 1024,      # 200KB
            'large_1.bin': 2 * 1024 * 1024,  # 2MB
            'subdir/nested.txt': 2048,
            'subdir/deep/file.bin': 10 * 1024,
            'empty.txt': 0,
            'one_byte.bin': 1,
        }

        if not self.quick:
            # Add larger files for full test
            self.test_files.update({
                'large_2.bin': 8 * 1024 * 1024,   # 8MB for chunking tests
            })

        print(f"Generating {len(self.test_files)} test files...")
        for relpath, size in self.test_files.items():
            filepath = self.local_source / relpath
            filepath.parent.mkdir(parents=True, exist_ok=True)
            filepath.write_bytes(generate_random_content(size))

        self.original_md5s = {}
        print("Computing MD5 checksums...")
        for relpath in self.test_files:
            self.original_md5s[relpath] = compute_md5(self.local_source / relpath)

        print("Copying to remote source...")
        shutil.copytree(self.local_source, self.remote_source, dirs_exist_ok=True)

        total_mb = sum(self.test_files.values()) / (1024*1024)
        print(f"Setup complete. Total: {total_mb:.2f} MB")

    def run_straws(self, args: list[str], timeout: int = 120) -> tuple[int, str, str]:
        cmd = [str(self.straws)] + args
        if self.verbose:
            print(f"    $ {' '.join(cmd)}")
        try:
            result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
            return result.returncode, result.stdout, result.stderr
        except subprocess.TimeoutExpired:
            return -1, '', 'Command timed out'

    def verify_transfer(self, dest_base: Path, expected_files: dict[str, int],
                       use_remote: bool = False) -> tuple[bool, str]:
        errors = []
        for relpath, expected_size in expected_files.items():
            dest_file = dest_base / relpath

            if use_remote:
                remote_path = str(dest_file)
                result = subprocess.run(
                    ['ssh', self.ssh_host, f'test -f "{remote_path}" && stat -f%z "{remote_path}" 2>/dev/null || stat -c%s "{remote_path}" 2>/dev/null'],
                    capture_output=True, text=True
                )
                if result.returncode != 0 or not result.stdout.strip():
                    errors.append(f"Missing: {relpath}")
                    continue
                actual_size = int(result.stdout.strip())
                if actual_size != expected_size:
                    errors.append(f"Size mismatch {relpath}: {expected_size} vs {actual_size}")
                    continue
                actual_md5 = compute_md5_remote(self.ssh_host, remote_path)
            else:
                if not dest_file.exists():
                    errors.append(f"Missing: {relpath}")
                    continue
                actual_size = dest_file.stat().st_size
                if actual_size != expected_size:
                    errors.append(f"Size mismatch {relpath}: {expected_size} vs {actual_size}")
                    continue
                actual_md5 = compute_md5(dest_file)

            if actual_md5 != self.original_md5s[relpath]:
                errors.append(f"MD5 mismatch {relpath}")

        return (False, '\n'.join(errors)) if errors else (True, '')

    def run_test(self, name: str, test_func) -> TestResult:
        print(f"  {color('TEST:', Colors.BOLD)} {name} ... ", end='')
        start = time.time()
        try:
            test_func()
            duration = time.time() - start
            result = TestResult(name=name, passed=True, duration=duration)
            print(f"{color('PASS', Colors.GREEN)} ({duration:.1f}s)")
        except Exception as e:
            duration = time.time() - start
            result = TestResult(name=name, passed=False, duration=duration, error=str(e))
            print(f"{color('FAIL', Colors.RED)}: {e}")
        self.results.append(result)
        return result

    def clean_dest(self):
        if self.local_dest.exists():
            shutil.rmtree(self.local_dest)
        self.local_dest.mkdir()
        if self.remote_dest.exists():
            shutil.rmtree(self.remote_dest)
        self.remote_dest.mkdir()

    # =========================================================================
    # Test Cases
    # =========================================================================

    def test_upload_single(self, tunnels: int, verify: bool):
        self.clean_dest()
        src = self.local_source / 'medium_1.bin'
        dest = f"{self.ssh_host}:{self.remote_dest}/medium_1.bin"
        args = ['-t', str(tunnels), '--no-progress', str(src), dest]
        if verify:
            args.insert(0, '--verify')
        rc, out, err = self.run_straws(args)
        assert rc == 0, f"Exit {rc}: {err}"
        ok, msg = self.verify_transfer(self.remote_dest, {'medium_1.bin': self.test_files['medium_1.bin']}, use_remote=True)
        assert ok, msg

    def test_upload_dir(self, tunnels: int, verify: bool):
        self.clean_dest()
        src = str(self.local_source) + '/'
        dest = f"{self.ssh_host}:{self.remote_dest}/"
        args = ['-t', str(tunnels), '--no-progress', src, dest]
        if verify:
            args.insert(0, '--verify')
        rc, out, err = self.run_straws(args)
        assert rc == 0, f"Exit {rc}: {err}"
        ok, msg = self.verify_transfer(self.remote_dest, self.test_files, use_remote=True)
        assert ok, msg

    def test_download_single(self, tunnels: int, verify: bool):
        self.clean_dest()
        src = f"{self.ssh_host}:{self.remote_source}/large_1.bin"
        dest = str(self.local_dest) + '/'
        args = ['-t', str(tunnels), '--no-progress', src, dest]
        if verify:
            args.insert(0, '--verify')
        rc, out, err = self.run_straws(args)
        assert rc == 0, f"Exit {rc}: {err}"
        ok, msg = self.verify_transfer(self.local_dest, {'large_1.bin': self.test_files['large_1.bin']}, use_remote=False)
        assert ok, msg

    def test_download_dir(self, tunnels: int, verify: bool):
        self.clean_dest()
        src = f"{self.ssh_host}:{self.remote_source}/"
        dest = str(self.local_dest) + '/'
        args = ['-t', str(tunnels), '--no-progress', src, dest]
        if verify:
            args.insert(0, '--verify')
        rc, out, err = self.run_straws(args)
        assert rc == 0, f"Exit {rc}: {err}"
        ok, msg = self.verify_transfer(self.local_dest, self.test_files, use_remote=False)
        assert ok, msg

    def test_chunking(self, tunnels: int):
        self.clean_dest()
        # Use large_2 with small chunks to force multiple chunks
        if 'large_2.bin' not in self.test_files:
            return  # Skip in quick mode
        src = self.local_source / 'large_2.bin'
        dest = f"{self.ssh_host}:{self.remote_dest}/large_2.bin"
        args = ['-t', str(tunnels), '--chunk-size', '2M', '--no-progress', '--verify', str(src), dest]
        rc, out, err = self.run_straws(args)
        assert rc == 0, f"Exit {rc}: {err}"
        ok, msg = self.verify_transfer(self.remote_dest, {'large_2.bin': self.test_files['large_2.bin']}, use_remote=True)
        assert ok, msg

    def test_edge_cases(self):
        self.clean_dest()
        # Empty file
        src = self.local_source / 'empty.txt'
        dest = f"{self.ssh_host}:{self.remote_dest}/empty.txt"
        rc, _, err = self.run_straws(['-t', '2', '--no-progress', str(src), dest])
        assert rc == 0, f"Empty file failed: {err}"

        # Single byte
        src = self.local_source / 'one_byte.bin'
        dest = f"{self.ssh_host}:{self.remote_dest}/one_byte.bin"
        rc, _, err = self.run_straws(['-t', '2', '--no-progress', str(src), dest])
        assert rc == 0, f"One byte file failed: {err}"

        ok, msg = self.verify_transfer(self.remote_dest, {'empty.txt': 0, 'one_byte.bin': 1}, use_remote=True)
        assert ok, msg

    def test_multiple_sources(self):
        self.clean_dest()
        sources = [str(self.local_source / f) for f in ['small_1.txt', 'small_2.bin', 'medium_1.bin']]
        dest = f"{self.ssh_host}:{self.remote_dest}/"
        args = ['-t', '4', '--no-progress'] + sources + [dest]
        rc, _, err = self.run_straws(args)
        assert rc == 0, f"Exit {rc}: {err}"
        expected = {k: v for k, v in self.test_files.items() if k in ['small_1.txt', 'small_2.bin', 'medium_1.bin']}
        ok, msg = self.verify_transfer(self.remote_dest, expected, use_remote=True)
        assert ok, msg

    def run_all_tests(self):
        print(f"\n{'=' * 60}")
        print(f"{color('STRAWS REGRESSION TEST SUITE', Colors.BOLD)}")
        print(f"{'=' * 60}")
        print(f"Binary: {self.straws}")
        print(f"SSH Host: {self.ssh_host}")
        print(f"Mode: {'Quick' if self.quick else 'Full'}")

        self.setup()

        # Tunnel counts to test
        tunnel_counts = [2, 4] if self.quick else [1, 2, 4, 8]

        # Upload tests
        print(f"\n{color('>>> UPLOAD TESTS (Local -> Remote) <<<', Colors.YELLOW)}")
        for t in tunnel_counts:
            self.run_test(f"Upload single file (t={t})", lambda t=t: self.test_upload_single(t, False))
            self.run_test(f"Upload single +verify (t={t})", lambda t=t: self.test_upload_single(t, True))
            # Only do full dir tests with more tunnels to keep it fast
            if t >= 2:
                self.run_test(f"Upload directory (t={t})", lambda t=t: self.test_upload_dir(t, False))
                self.run_test(f"Upload dir +verify (t={t})", lambda t=t: self.test_upload_dir(t, True))

        # Download tests
        print(f"\n{color('>>> DOWNLOAD TESTS (Remote -> Local) <<<', Colors.YELLOW)}")
        for t in tunnel_counts:
            self.run_test(f"Download single file (t={t})", lambda t=t: self.test_download_single(t, False))
            self.run_test(f"Download single +verify (t={t})", lambda t=t: self.test_download_single(t, True))
            if t >= 2:
                self.run_test(f"Download directory (t={t})", lambda t=t: self.test_download_dir(t, False))
                self.run_test(f"Download dir +verify (t={t})", lambda t=t: self.test_download_dir(t, True))

        # Edge cases
        print(f"\n{color('>>> EDGE CASE TESTS <<<', Colors.YELLOW)}")
        self.run_test("Empty and 1-byte files", self.test_edge_cases)
        self.run_test("Multiple source files", self.test_multiple_sources)
        if not self.quick:
            self.run_test("Large file chunking (t=4, 2M chunks)", lambda: self.test_chunking(4))

        self.print_summary()

    def print_summary(self):
        print(f"\n{'=' * 60}")
        print(f"{color('SUMMARY', Colors.BOLD)}")
        print(f"{'=' * 60}")

        passed = sum(1 for r in self.results if r.passed)
        failed = len(self.results) - passed
        total_time = sum(r.duration for r in self.results)

        print(f"Total: {len(self.results)} | {color(f'Passed: {passed}', Colors.GREEN)}", end='')
        if failed:
            print(f" | {color(f'Failed: {failed}', Colors.RED)}")
            for r in self.results:
                if not r.passed:
                    print(f"  - {r.name}: {r.error}")
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
    parser = argparse.ArgumentParser(description='Straws regression tests')
    parser.add_argument('--keep-temp', action='store_true', help="Keep temp dirs")
    parser.add_argument('--verbose', '-v', action='store_true', help='Show commands')
    parser.add_argument('--straws', default='./target/release/straws', help='Binary path')
    parser.add_argument('--host', default='localhost', help='SSH host')
    parser.add_argument('--quick', '-q', action='store_true', help='Quick test subset')
    args = parser.parse_args()

    straws_path = Path(args.straws)
    if not straws_path.exists():
        straws_path = Path(__file__).parent / args.straws
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

    suite = StrawsTestSuite(str(straws_path), args.host, args.verbose, args.keep_temp, args.quick)
    try:
        suite.run_all_tests()
        sys.exit(0 if all(r.passed for r in suite.results) else 1)
    except KeyboardInterrupt:
        print(f"\n{color('Interrupted', Colors.YELLOW)}")
        sys.exit(130)


if __name__ == '__main__':
    main()
