/// Embedded Python agent code
/// This agent runs on the remote host via SSH and handles file operations
pub const PYTHON_AGENT: &str = r#"
import sys, os, struct, hashlib

CHUNK = 65536  # 64KB streaming chunks

# Simple write file handle cache: keep the last written file open
_write_cache_path = None
_write_cache_fh = None

def _get_write_fh(path):
    """Get a file handle for writing, reusing cached handle if same path."""
    global _write_cache_path, _write_cache_fh
    if _write_cache_path == path and _write_cache_fh is not None:
        return _write_cache_fh
    # Close previous handle
    if _write_cache_fh is not None:
        try:
            _write_cache_fh.close()
        except Exception:
            pass
    # Open existing file for read+write, or create new file
    if os.path.exists(path):
        _write_cache_fh = open(path, 'r+b')
    else:
        _write_cache_fh = open(path, 'wb')
    _write_cache_path = path
    return _write_cache_fh

def _close_write_cache():
    """Close the cached write handle."""
    global _write_cache_path, _write_cache_fh
    if _write_cache_fh is not None:
        try:
            _write_cache_fh.close()
        except Exception:
            pass
    _write_cache_fh = None
    _write_cache_path = None

def read_exact(n):
    """Read exactly n bytes from stdin"""
    data = b''
    while len(data) < n:
        chunk = sys.stdin.buffer.read(n - len(data))
        if not chunk:
            return None
        data += chunk
    return data

def write_response(status, data):
    """Write response: status(1) + data_len(8) + data"""
    sys.stdout.buffer.write(struct.pack('>B', status))
    sys.stdout.buffer.write(struct.pack('>Q', len(data)))

    # Stream data in chunks
    offset = 0
    while offset < len(data):
        end = min(offset + CHUNK, len(data))
        sys.stdout.buffer.write(data[offset:end])
        offset = end
    sys.stdout.buffer.flush()

def write_error(msg):
    """Write error response"""
    write_response(1, msg.encode('utf-8')[:1000])

def handle_read(path, offset, length):
    """Read bytes from file"""
    try:
        with open(path, 'rb') as f:
            # Get file size without extra seeks using fstat
            file_size = os.fstat(f.fileno()).st_size
            actual_len = min(length, max(0, file_size - offset))
            f.seek(offset)

            # Send success header first
            sys.stdout.buffer.write(struct.pack('>B', 0))  # status = success
            sys.stdout.buffer.write(struct.pack('>Q', actual_len))

            # Stream data in chunks
            remaining = actual_len
            while remaining > 0:
                chunk_size = min(CHUNK, remaining)
                data = f.read(chunk_size)
                if not data:
                    break
                sys.stdout.buffer.write(data)
                remaining -= len(data)
            sys.stdout.buffer.flush()
    except FileNotFoundError:
        write_error(f'File not found: {path}')
    except PermissionError:
        write_error(f'Permission denied: {path}')
    except Exception as e:
        write_error(str(e))

def handle_write(path, offset, length, data):
    """Write bytes to file at offset"""
    try:
        # Ensure parent directory exists
        parent = os.path.dirname(path)
        if parent and not os.path.exists(parent):
            os.makedirs(parent, exist_ok=True)

        # Use cached file handle for repeated writes to same file
        f = _get_write_fh(path)
        f.seek(offset)
        f.write(data)
        f.flush()
        write_response(0, b'')
    except Exception as e:
        _close_write_cache()
        write_error(str(e))

def handle_md5(path, offset, length):
    """Compute MD5 of byte range"""
    try:
        md5 = hashlib.md5()
        with open(path, 'rb') as f:
            f.seek(offset)
            remaining = length
            while remaining > 0:
                chunk_size = min(CHUNK, remaining)
                data = f.read(chunk_size)
                if not data:
                    break
                md5.update(data)
                remaining -= len(data)
        write_response(0, md5.hexdigest().encode('utf-8'))
    except FileNotFoundError:
        write_error(f'File not found: {path}')
    except PermissionError:
        write_error(f'Permission denied: {path}')
    except Exception as e:
        write_error(str(e))

def handle_mkdir(path):
    """Create directory recursively"""
    try:
        os.makedirs(path, exist_ok=True)
        write_response(0, b'')
    except Exception as e:
        write_error(str(e))

def handle_stat(path):
    """Get file size, mode, mtime"""
    try:
        st = os.stat(path)
        # size(8) + mode(4) + mtime(8)
        data = struct.pack('>QIQ', st.st_size, st.st_mode, int(st.st_mtime))
        write_response(0, data)
    except FileNotFoundError:
        write_error(f'File not found: {path}')
    except PermissionError:
        write_error(f'Permission denied: {path}')
    except Exception as e:
        write_error(str(e))

def handle_truncate(path, size):
    """Truncate/preallocate file to size"""
    try:
        # Close cached write handle if it's for this file (we're about to truncate)
        _close_write_cache()

        # Ensure parent directory exists
        parent = os.path.dirname(path)
        if parent and not os.path.exists(parent):
            os.makedirs(parent, exist_ok=True)

        with open(path, 'ab') as f:
            f.truncate(size)
        write_response(0, b'')
    except Exception as e:
        write_error(str(e))

def compute_file_md5(fpath):
    """Compute MD5 hash of a file, return 32-char hex string."""
    h = hashlib.md5()
    try:
        with open(fpath, 'rb') as f:
            while True:
                chunk = f.read(65536)
                if not chunk:
                    break
                h.update(chunk)
        return h.hexdigest()
    except Exception:
        return None

def handle_find(base_path, with_md5=False):
    """Recursively find all files and stream their stats.
    Response format: status(1) then streamed entries, each:
      path_len(2) + path(utf8) + size(8) + mode(4) + mtime(8) [+ md5(32) if with_md5]
    Paths are returned relative to base_path (preserving the user's path format).
    Terminated by path_len=0.
    """
    try:
        # Expand ~ but keep the path format for consistent prefix stripping on client
        base = os.path.expanduser(base_path)
        sys.stdout.buffer.write(struct.pack('>B', 0))  # success status
        sys.stdout.buffer.flush()

        entry_count = 0
        FLUSH_INTERVAL = 1000  # Batch flushes for better performance

        def send_entry(rel_path, st, fpath):
            """Send entry with path relative to user-provided base_path"""
            nonlocal entry_count
            # Combine user's original base_path with relative portion
            full_path = os.path.join(base_path, rel_path) if rel_path else base_path
            path_bytes = full_path.encode('utf-8')
            sys.stdout.buffer.write(struct.pack('>H', len(path_bytes)))
            sys.stdout.buffer.write(path_bytes)
            sys.stdout.buffer.write(struct.pack('>QIQ', st.st_size, st.st_mode, int(st.st_mtime)))
            if with_md5:
                md5_hex = compute_file_md5(fpath)
                if md5_hex is None:
                    md5_hex = '0' * 32
                sys.stdout.buffer.write(md5_hex.encode('ascii'))
            entry_count += 1
            if entry_count % FLUSH_INTERVAL == 0:
                sys.stdout.buffer.flush()

        if os.path.isfile(base):
            # Single file - return with original base_path
            send_entry('', os.stat(base), base)
        else:
            for root, dirs, filenames in os.walk(base):
                for fname in filenames:
                    fpath = os.path.join(root, fname)
                    try:
                        st = os.stat(fpath)
                        if os.path.isfile(fpath):
                            # Get path relative to expanded base, then join with original base_path
                            rel = os.path.relpath(fpath, base)
                            send_entry(rel, st, fpath)
                    except (OSError, IOError):
                        continue

        # End marker: path_len=0
        sys.stdout.buffer.write(struct.pack('>H', 0))
        sys.stdout.buffer.flush()
    except Exception as e:
        write_error(str(e))

def main():
    while True:
        # Read request header: op(1) + path_len(2)
        header = read_exact(3)
        if header is None:
            break

        op = header[0]
        path_len = struct.unpack('>H', header[1:3])[0]

        # Read path
        path_bytes = read_exact(path_len)
        if path_bytes is None:
            break

        try:
            path = path_bytes.decode('utf-8')
        except UnicodeDecodeError:
            write_error('Invalid UTF-8 in path')
            continue

        # Expand ~ to home directory (but preserve relative paths)
        path = os.path.expanduser(path)

        # Read offset(8) + length(8)
        nums = read_exact(16)
        if nums is None:
            break

        offset, length = struct.unpack('>QQ', nums)

        # Handle operation
        if op == 0:  # READ
            handle_read(path, offset, length)
        elif op == 1:  # WRITE
            # Read data for write
            data = read_exact(length) if length > 0 else b''
            if data is None and length > 0:
                break
            handle_write(path, offset, length, data)
        elif op == 2:  # MD5
            # Close write cache before computing MD5 to ensure all data is flushed
            _close_write_cache()
            handle_md5(path, offset, length)
        elif op == 3:  # MKDIR
            handle_mkdir(path)
        elif op == 4:  # STAT
            handle_stat(path)
        elif op == 5:  # TRUNCATE
            handle_truncate(path, length)
        elif op == 6:  # FIND
            handle_find(path, offset != 0)  # offset=1 means compute MD5s
        else:
            write_error(f'Unknown operation: {op}')

if __name__ == '__main__':
    try:
        main()
    except Exception as e:
        sys.stderr.write(f'Agent error: {e}\n')
        sys.exit(1)
"#;

/// Returns the Python agent code as a single-line command suitable for SSH exec
pub fn agent_command() -> &'static str {
    use std::sync::OnceLock;
    static CACHED_COMMAND: OnceLock<String> = OnceLock::new();
    CACHED_COMMAND.get_or_init(|| {
        let escaped = PYTHON_AGENT
            .replace('\\', "\\\\")
            .replace('\'', "'\"'\"'");
        format!("exec python3 -c '{}'", escaped)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_command() {
        let cmd = agent_command();
        assert!(cmd.starts_with("exec python3 -c '"));
        assert!(cmd.contains("def main()"));
    }

    #[test]
    fn test_agent_command_cached() {
        // Verify caching works - same pointer returned
        let cmd1 = agent_command();
        let cmd2 = agent_command();
        assert!(std::ptr::eq(cmd1, cmd2));
    }
}
