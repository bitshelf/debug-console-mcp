//! statusline-watch — inotify daemon that mirrors MCP cache files to /dev/shm.
//!
//! Watches .dut-serial/statusline-cache for one or more projects. On any change,
//! copies the file content to /dev/shm/claude-status-<project_hash> so the
//! Python statusline hook can read it with a single `cat` (1 syscall, zero Python).
//!
//! Usage:
//!   statusline-watch --project-dir /path/to/project1 --project-dir /path/to/project2
//!
//! The MCP writes statusline-cache directly. This daemon is a backup for when
//! some other process updates the file without writing to /dev/shm.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use inotify::{Inotify, WatchMask};

/// Compute the same 8-char project hash as the MCP and Python hook.
fn project_hash(project_dir: &Path) -> String {
    use md5::{Digest, Md5};
    let canonical = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());
    let mut hasher = Md5::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    format!("{:08x}", digest).chars().take(8).collect()
}

fn shm_cache_path(project_dir: &Path) -> PathBuf {
    let shm_dir = if Path::new("/dev/shm").is_dir() {
        "/dev/shm"
    } else {
        "/tmp"
    };
    PathBuf::from(format!("{}/claude-status-{}", shm_dir, project_hash(project_dir)))
}

fn source_cache_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".dut-serial").join("statusline-cache")
}

/// Copy source to /dev/shm on startup (file may already have content).
fn initial_sync(project_dir: &Path) {
    let src = source_cache_path(project_dir);
    let dst = shm_cache_path(project_dir);
    if src.exists()
        && let Ok(content) = std::fs::read(&src)
        && !content.is_empty()
    {
        let _ = std::fs::write(&dst, &content);
    }
}

fn main() {
    let mut project_dirs: Vec<PathBuf> = Vec::new();

    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--project-dir" => {
                i += 1;
                if i < args.len() {
                    project_dirs.push(PathBuf::from(&args[i]));
                }
            }
            "--registry" => {
                i += 1;
                if i < args.len() {
                    if let Ok(content) = std::fs::read_to_string(&args[i]) {
                        for line in content.lines() {
                            let line = line.trim();
                            if !line.is_empty() {
                                project_dirs.push(PathBuf::from(line));
                            }
                        }
                    }
                }
            }
            _ => {
                eprintln!("Usage: statusline-watch --project-dir <PATH> [--project-dir <PATH> ...]");
                eprintln!("       statusline-watch --registry <FILE>");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    if project_dirs.is_empty() {
        eprintln!("statusline-watch: no projects specified, nothing to watch");
        std::process::exit(0);
    }

    // Setup inotify
    let mut inotify = Inotify::init().expect("inotify_init failed");

    // Map watch descriptor → (project_dir, cache_file_path)
    let mut watches: HashMap<inotify::WatchDescriptor, PathBuf> = HashMap::new();

    for proj in &project_dirs {
        let src = source_cache_path(proj);
        // Create .dut-serial if it doesn't exist
        if let Some(parent) = src.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Create the file if it doesn't exist (so inotify can watch it)
        if !src.exists() {
            let _ = std::fs::write(&src, b"");
        }

        match inotify.watches().add(
            &src,
            WatchMask::MODIFY | WatchMask::CLOSE_WRITE | WatchMask::CREATE,
        ) {
            Ok(wd) => {
                watches.insert(wd, proj.clone());
                initial_sync(proj);
                eprintln!(
                    "statusline-watch: watching {}",
                    src.display()
                );
            }
            Err(e) => {
                eprintln!("statusline-watch: cannot watch {}: {e}", src.display());
            }
        }
    }

    if watches.is_empty() {
        eprintln!("statusline-watch: no watches established, exiting");
        std::process::exit(1);
    }

    eprintln!(
        "statusline-watch: watching {} project(s), pid={}",
        watches.len(),
        std::process::id()
    );

    // Event loop
    let mut buffer = [0u8; 4096];
    loop {
        match inotify.read_events_blocking(&mut buffer) {
            Ok(events) => {
                for event in events {
                    if let Some(proj) = watches.get(&event.wd) {
                        let src = source_cache_path(proj);
                        if let Ok(content) = std::fs::read(&src)
                            && !content.is_empty()
                        {
                            let dst = shm_cache_path(proj);
                            if let Err(e) = std::fs::write(&dst, &content) {
                                eprintln!(
                                    "statusline-watch: write to {} failed: {e}",
                                    dst.display()
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("statusline-watch: inotify error: {e}");
                break;
            }
        }
    }
}
