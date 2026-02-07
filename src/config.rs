// Copyright (c) 2025, Joe Drago <joedrago@gmail.com>
// SPDX-License-Identifier: BSD-2-Clause

use crate::error::{Result, StrawsError};
use crate::job::scheduler::MIN_CHUNK_SIZE;
use clap::Parser;
use std::path::PathBuf;

fn version_string() -> &'static str {
    concat!(env!("CARGO_PKG_VERSION"), " (", env!("GIT_SHA"), ")")
}

/// High-performance SSH file transfer tool
#[derive(Parser, Debug, Clone)]
#[command(name = "straws")]
#[command(author, version = version_string(), about, long_about = None)]
pub struct Args {
    /// Source path(s) - can be local or remote (user@host:path)
    #[arg(required = true)]
    pub sources: Vec<String>,

    /// Destination path - can be local or remote (user@host:path)
    #[arg(required = true)]
    pub destination: String,

    /// Number of parallel SSH connections
    #[arg(short = 't', long = "tunnels", default_value = "8")]
    pub tunnels: usize,

    /// SSH port
    #[arg(short = 'P', long = "port", default_value = "22")]
    pub port: u16,

    /// SSH private key file
    #[arg(short = 'i', long = "identity")]
    pub identity: Option<PathBuf>,

    /// Prompt for password
    #[arg(long = "password")]
    pub password_prompt: bool,

    /// Read password from file
    #[arg(long = "password-file")]
    pub password_file: Option<PathBuf>,

    /// Environment variable containing password
    #[arg(long = "password-env", default_value = "STRAWS_PASSWORD")]
    pub password_env: String,

    /// Enable SSH compression
    #[arg(short = 'c', long = "compress")]
    pub compress: bool,

    /// Chunk size for large files (supports K, M, G suffixes)
    #[arg(long = "chunk-size", default_value = "50M", value_parser = parse_size)]
    pub chunk_size: u64,

    /// Enable MD5 verification
    #[arg(long = "verify")]
    pub verify: bool,

    /// Force transfer even if file already exists
    #[arg(short = 'f', long = "force")]
    pub force: bool,

    /// Disable progress display
    #[arg(long = "no-progress")]
    pub no_progress: bool,

    /// Debug log file (line-buffered)
    #[arg(long = "debug-log")]
    pub debug_log: Option<PathBuf>,

    /// Show detailed tunnel status
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,
}

fn parse_size(s: &str) -> std::result::Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size".to_string());
    }

    let (num_str, multiplier) = if let Some(n) = s.strip_suffix(['K', 'k']) {
        (n, 1024u64)
    } else if let Some(n) = s.strip_suffix(['M', 'm']) {
        (n, 1024 * 1024)
    } else if let Some(n) = s.strip_suffix(['G', 'g']) {
        (n, 1024 * 1024 * 1024)
    } else {
        (s, 1)
    };

    num_str
        .parse::<u64>()
        .map(|n| n * multiplier)
        .map_err(|e| format!("invalid size '{}': {}", s, e))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Download,
    Upload,
}

#[derive(Debug, Clone)]
pub struct RemoteSpec {
    pub user: Option<String>,
    pub host: String,
    pub path: String,
}

impl RemoteSpec {
    pub fn parse(s: &str) -> Option<Self> {
        // Format: [user@]host:path
        let colon_pos = s.find(':')?;

        // Make sure this isn't a Windows path like C:\
        if colon_pos == 1 && s.chars().next()?.is_ascii_alphabetic() {
            return None;
        }

        let host_part = &s[..colon_pos];
        let path = s[colon_pos + 1..].to_string();

        let (user, host) = if let Some(at_pos) = host_part.find('@') {
            (
                Some(host_part[..at_pos].to_string()),
                host_part[at_pos + 1..].to_string(),
            )
        } else {
            (None, host_part.to_string())
        };

        Some(RemoteSpec { user, host, path })
    }

    pub fn user_host(&self) -> String {
        match &self.user {
            Some(u) => format!("{}@{}", u, self.host),
            None => self.host.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub direction: Direction,
    pub remote: RemoteSpec,
    pub local_paths: Vec<PathBuf>,
    pub tunnels: usize,
    pub port: u16,
    pub identity: Option<PathBuf>,
    pub password: Option<String>,
    pub compress: bool,
    pub chunk_size: u64,
    pub verify: bool,
    pub force: bool,
    pub no_progress: bool,
    pub debug_log: Option<PathBuf>,
    pub verbose: bool,
}

impl Config {
    pub fn from_args(args: Args, password: Option<String>) -> Result<Self> {
        // Determine direction by checking which side is remote
        let dest_remote = RemoteSpec::parse(&args.destination);
        let source_remotes: Vec<_> = args.sources.iter().filter_map(|s| RemoteSpec::parse(s)).collect();

        let (direction, remote, local_paths) = match (source_remotes.first(), dest_remote) {
            (Some(src_remote), None) => {
                // Download: remote source, local destination
                if source_remotes.len() > 1 {
                    // All sources must be from same host
                    let first_host = &src_remote.host;
                    for r in &source_remotes {
                        if r.host != *first_host {
                            return Err(StrawsError::Config(
                                "All remote sources must be from the same host".to_string(),
                            ));
                        }
                    }
                }
                let remote_paths: Vec<_> = source_remotes.iter().map(|r| r.path.clone()).collect();
                let mut combined_remote = src_remote.clone();
                // For multiple sources, we'll handle them in job scheduling
                if remote_paths.len() == 1 {
                    combined_remote.path = remote_paths[0].clone();
                } else {
                    // Store multiple paths separated by null byte (internal use)
                    combined_remote.path = remote_paths.join("\0");
                }
                (
                    Direction::Download,
                    combined_remote,
                    vec![PathBuf::from(&args.destination)],
                )
            }
            (None, Some(dest_remote)) => {
                // Upload: local sources, remote destination
                let local_paths: Vec<PathBuf> = args.sources.iter().map(PathBuf::from).collect();
                (Direction::Upload, dest_remote, local_paths)
            }
            (Some(_), Some(_)) => {
                return Err(StrawsError::Config(
                    "Cannot transfer between two remote hosts".to_string(),
                ));
            }
            (None, None) => {
                return Err(StrawsError::Config(
                    "At least one path must be remote (user@host:path)".to_string(),
                ));
            }
        };

        if args.tunnels == 0 {
            return Err(StrawsError::Config("Tunnel count must be at least 1".to_string()));
        }

        Ok(Config {
            direction,
            remote,
            local_paths,
            tunnels: args.tunnels,
            port: args.port,
            identity: args.identity,
            password,
            compress: args.compress,
            chunk_size: args.chunk_size.max(MIN_CHUNK_SIZE),
            verify: args.verify,
            force: args.force,
            no_progress: args.no_progress,
            debug_log: args.debug_log,
            verbose: args.verbose,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("16K").unwrap(), 16 * 1024);
        assert_eq!(parse_size("16M").unwrap(), 16 * 1024 * 1024);
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("16m").unwrap(), 16 * 1024 * 1024);
    }

    #[test]
    fn test_remote_spec_parse() {
        let spec = RemoteSpec::parse("user@host:path/to/file").unwrap();
        assert_eq!(spec.user, Some("user".to_string()));
        assert_eq!(spec.host, "host");
        assert_eq!(spec.path, "path/to/file");

        let spec = RemoteSpec::parse("host:/path").unwrap();
        assert_eq!(spec.user, None);
        assert_eq!(spec.host, "host");
        assert_eq!(spec.path, "/path");

        // Not a remote path
        assert!(RemoteSpec::parse("/local/path").is_none());
    }
}
