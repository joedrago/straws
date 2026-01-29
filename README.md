# straws

A high-performance SSH file transfer tool that uses multiple parallel connections to maximize throughput.

## Features

- **Parallel transfers** - Uses multiple simultaneous SSH connections to saturate your bandwidth
- **Bidirectional** - Upload (local → remote) and download (remote → local)
- **Large file chunking** - Splits large files across multiple connections for faster transfers
- **Recursive directory support** - Transfer entire directory trees
- **Data integrity** - Optional MD5 verification ensures files transfer correctly
- **Progress display** - Real-time progress with speed, ETA, and per-connection status
- **Resume-friendly** - Writes to temp files, renamed on completion
- **Cross-platform** - Works on Linux and macOS

## Installation

### From source

```bash
git clone https://github.com/yourusername/straws.git
cd straws
cargo build --release
# Binary is at ./target/release/straws
```

## Quick Start

```bash
# Upload a file
straws myfile.txt user@server:/path/to/destination/

# Download a file
straws user@server:/path/to/file.txt ./local/destination/

# Upload a directory recursively
straws ./local/directory/ user@server:/remote/path/

# Download with 16 parallel connections
straws -t 16 user@server:/large/file.bin ./

# Transfer with MD5 verification
straws --verify ./important.dat user@server:/backup/
```

## Usage

```
straws [OPTIONS] <SOURCES>... <DESTINATION>
```

### Arguments

| Argument | Description |
|----------|-------------|
| `SOURCES` | One or more source paths. Can be local paths or remote paths in `user@host:path` format. |
| `DESTINATION` | Destination path. Can be local or remote. Use trailing `/` for directories. |

### Options

#### Connection Options

| Option | Default | Description |
|--------|---------|-------------|
| `-t, --tunnels <N>` | 8 | Number of parallel SSH connections. More tunnels can increase throughput but use more resources. |
| `-P, --port <PORT>` | 22 | SSH port to connect to. |
| `-i, --identity <FILE>` | - | Path to SSH private key file. |
| `-c, --compress` | off | Enable SSH compression. Useful for compressible data over slow links. |

#### Authentication

| Option | Description |
|--------|-------------|
| `--password` | Prompt interactively for password. |
| `--password-file <FILE>` | Read password from a file (first line). |
| `--password-env <VAR>` | Environment variable containing the password. Default: `STRAWS_PASSWORD` |

By default, straws uses SSH key-based authentication. If you need password authentication, use one of the password options above.

#### Transfer Options

| Option | Default | Description |
|--------|---------|-------------|
| `--chunk-size <SIZE>` | 16M | Size threshold for splitting large files across multiple connections. Supports suffixes: `K` (kilobytes), `M` (megabytes), `G` (gigabytes). |
| `--verify` | off | Compute and compare MD5 checksums after transfer to verify data integrity. |

#### Display Options

| Option | Description |
|--------|-------------|
| `--no-progress` | Disable the progress display. Useful for scripts or logging. |
| `-v, --verbose` | Show detailed per-tunnel status in the progress display. |
| `--debug-log <FILE>` | Write detailed debug information to a file. |

#### Other

| Option | Description |
|--------|-------------|
| `-h, --help` | Print help information. |
| `-V, --version` | Print version. |

## Examples

### Basic transfers

```bash
# Upload single file
straws report.pdf user@server:~/documents/

# Download single file
straws user@server:/var/log/app.log ./logs/

# Upload multiple files
straws file1.txt file2.txt file3.txt user@server:/destination/

# Upload entire directory
straws ./project/ user@server:/backup/project/
```

### Performance tuning

```bash
# Use more parallel connections for high-bandwidth links
straws -t 16 largefile.iso user@server:/files/

# Use fewer connections for limited servers
straws -t 2 data.tar.gz user@server:/backup/

# Smaller chunks for more parallelism on large files
straws -t 8 --chunk-size 4M hugefile.bin user@server:/data/

# Enable compression for text/compressible data
straws -c ./logs/ user@server:/archive/
```

### Verification and reliability

```bash
# Verify transfer integrity with MD5
straws --verify critical-data.db user@server:/backup/

# Transfer with verification and detailed logging
straws --verify --debug-log transfer.log ./data/ user@server:/backup/
```

### Authentication

```bash
# Use specific SSH key
straws -i ~/.ssh/deploy_key ./app/ user@server:/deploy/

# Use password from environment
export STRAWS_PASSWORD='mypassword'
straws ./files/ user@server:/destination/

# Prompt for password
straws --password ./files/ user@server:/destination/

# Custom SSH port
straws -P 2222 ./files/ user@server:/destination/
```

### Scripting

```bash
# Disable progress for cron jobs
straws --no-progress ./backup/ user@server:/backups/$(date +%Y%m%d)/

# Capture debug output for troubleshooting
straws --no-progress --debug-log /var/log/straws.log ./data/ user@server:/sync/
```

## How It Works

straws achieves high throughput by:

1. **Multiple SSH connections** - Opens N parallel SSH sessions to the remote host, each running a lightweight Python agent
2. **Intelligent chunking** - Large files are split into chunks that transfer simultaneously across different connections
3. **Streaming protocol** - Uses a binary protocol over SSH stdin/stdout for efficient data transfer
4. **Parallel file handling** - Multiple smaller files transfer concurrently across available connections

The tool automatically determines transfer direction based on which side has the remote path (`user@host:path` format).

## Requirements

- **Local**: Rust toolchain for building, or pre-built binary
- **Remote**: Python 3 and SSH server (standard on most Linux/Unix systems)
- **SSH access**: Key-based authentication recommended, password authentication supported

## Comparison with other tools

| Feature | straws | scp | rsync |
|---------|--------|-----|-------|
| Parallel connections | Yes (configurable) | No | No |
| Large file chunking | Yes | No | No |
| Progress display | Yes | Yes | Yes |
| MD5 verification | Optional | No | Yes (checksum) |
| Delta sync | No | No | Yes |
| Compression | Optional | Optional | Yes |

straws is optimized for **raw transfer speed** when you need to move large amounts of data quickly. For incremental backups or syncing with delta transfers, rsync may be more appropriate.

## Troubleshooting

### Transfer seems slow

- Increase tunnel count: `-t 16` or higher
- Check network bandwidth between hosts
- For compressible data, try `-c` flag

### Connection errors

- Verify SSH access: `ssh user@host echo ok`
- Check SSH key permissions: `chmod 600 ~/.ssh/id_rsa`
- Try with debug logging: `--debug-log debug.txt`

### "Python not found" on remote

The remote host needs Python 3 available as `python3`. Install it with your package manager:
```bash
# Debian/Ubuntu
sudo apt install python3

# RHEL/CentOS
sudo yum install python3

# macOS (usually pre-installed)
brew install python3
```

## License

BSD 2-Clause License - see [LICENSE](LICENSE) for details.
