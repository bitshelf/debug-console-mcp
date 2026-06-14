//! Host:port mutual-exclusion lock — O_EXCL atomic creation + zombie cleanup.

use std::path::Path;

/// Check project-level singleton: if an active MCP process already holds
/// a PID file under `project_dir/.dut-serial/`, return its PID.
///
/// Checks `.dut-serial/mcp.pid` first, then per-DUT subdirectories.
pub fn check_project_singleton(project_dir: &Path, dut_dir: &str) -> Option<u32> {
    let base = project_dir.join(dut_dir);

    // Collect all PID file paths to check
    let mut pid_files: Vec<std::path::PathBuf> = vec![base.join("mcp.pid")];
    // Also check per-DUT subdirectories
    if let Ok(entries) = std::fs::read_dir(&base) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let pid = path.join("mcp.pid");
                if pid.exists() {
                    pid_files.push(pid);
                }
            }
        }
    }

    for pid_file in &pid_files {
        let content = match std::fs::read_to_string(pid_file) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let pid: u32 = match content.trim().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if process_alive(pid) {
            if let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) {
                if comm.contains("debug-console") {
                    return Some(pid);
                }
            }
        }
        // Stale PID file → clean up.
        let _ = std::fs::remove_file(pid_file);
    }
    None
}

/// Acquire the lock for `host:target` (`target` may be a TCP port or device path).
/// Returns `None` on success, `Some(pid)` if a conflict exists.
///
/// Uses a bounded retry loop (max 8 attempts) instead of unbounded recursion
/// to prevent stack overflow under persistent races.
pub fn acquire_lock(host: &str, target: &str, lock_dir: &str) -> Option<u32> {
    let lock_key = format!("{:x}", fnv1a_hash(&format!("{host}:{target}")))[..8].to_string();
    let lock_dir_path = Path::new(lock_dir);
    let lock_path = lock_dir_path.join(format!("{lock_key}.lock"));

    std::fs::create_dir_all(lock_dir_path).ok();

    const MAX_RETRIES: usize = 8;
    for _ in 0..MAX_RETRIES {
        // Check existing lock.
        if lock_path.exists() {
            if let Some(conflicting_pid) = check_existing_lock(&lock_path) {
                return Some(conflicting_pid);
            }
            // Stale lock → clean up.
            std::fs::remove_file(&lock_path).ok();
        }

        // O_EXCL atomic creation.
        match try_create_lock(&lock_path, host, target) {
            Ok(()) => return None,
            Err(_) => {
                // Race: another process created the lock first.
                if let Some(pid) = check_existing_lock(&lock_path) {
                    return Some(pid);
                }
                // The winner disappeared between create and check — retry.
                std::fs::remove_file(&lock_path).ok();
                continue;
            }
        }
    }
    // Exhausted retries — report a conflict with our own PID as a fallback.
    Some(std::process::id())
}

/// Release the lock for `host:target`.
pub fn release_lock(host: &str, target: &str, lock_dir: &str) {
    let lock_key = format!("{:x}", fnv1a_hash(&format!("{host}:{target}")))[..8].to_string();
    let lock_path = Path::new(lock_dir).join(format!("{lock_key}.lock"));
    std::fs::remove_file(lock_path).ok();
}

fn check_existing_lock(lock_path: &Path) -> Option<u32> {
    let content = std::fs::read_to_string(lock_path).ok()?;
    let pid_str = content.lines().next()?;
    let pid: u32 = pid_str.trim().parse().ok()?;
    if process_alive(pid) { Some(pid) } else { None }
}

// Minimal libc bindings — avoid a `nix`/`libc` dependency for two syscalls.
// `C-unwind` is used so an unwind across the FFI boundary is safe under
// Rust 2024 edition (per Cargo.toml).
unsafe extern "C-unwind" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn __errno_location() -> *mut i32;
}

/// Check whether a process is alive.
///
/// `kill(pid, 0)` returns 0 if the process exists AND the caller has
/// permission to signal it. If the process exists but the caller lacks
/// permission, `errno` is set to `EPERM` (= 1 on Linux) and `kill` returns
/// -1 — the process is still alive. We treat both cases as alive to avoid
/// mistaking another user's process for a zombie and deleting its lock (which
/// would defeat mutual exclusion).
pub fn process_alive(pid: u32) -> bool {
    // SAFETY: kill(2) with sig=0 is a standard existence check on Unix.
    let ret = unsafe { kill(pid as i32, 0) };
    if ret == 0 {
        return true;
    }
    // Check errno for EPERM (process exists, permission denied).
    // SAFETY: __errno_location returns a pointer to a thread-local int; reading
    // it is safe in a single-threaded context here.
    let errno = unsafe { *__errno_location() };
    errno == 1 // EPERM on Linux
}

fn try_create_lock(lock_path: &Path, host: &str, target: &str) -> Result<(), ()> {
    use std::os::unix::fs::OpenOptionsExt;
    let pid = std::process::id();
    let timestamp = chrono::Local::now().to_rfc3339();
    let hostname = hostname();
    let content = format!("{pid}\n{host}:{target}\n{hostname}\n{timestamp}");

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(lock_path)
        .map_err(|_| ())?;

    use std::io::Write;
    let mut file = file;
    file.write_all(content.as_bytes()).map_err(|_| ())?;
    // Restrict lock file permissions to owner-only (0600).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(lock_path)
            .ok()
            .map(|m| m.permissions())
            .unwrap_or_else(|| std::fs::Permissions::from_mode(0o600));
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(lock_path, perms);
    }
    Ok(())
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// FNV-1a 64-bit hash — fast non-cryptographic hash for lock key uniqueness.
/// The `host:port` space is small (hundreds), so collision probability is
/// negligible.
fn fnv1a_hash(input: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_lock_dir() -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("embedded-debug-test-{}-{}", std::process::id(), id));
        // Clean up any old directory.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn acquire_and_release() {
        let dir = temp_lock_dir();
        let dir_str = dir.to_str().unwrap();
        assert!(acquire_lock("192.168.1.1", "2000", dir_str).is_none());
        release_lock("192.168.1.1", "2000", dir_str);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn different_ports_no_conflict() {
        let dir = temp_lock_dir();
        let dir_str = dir.to_str().unwrap();
        assert!(acquire_lock("192.168.1.1", "2000", dir_str).is_none());
        assert!(acquire_lock("192.168.1.1", "2001", dir_str).is_none());
        release_lock("192.168.1.1", "2000", dir_str);
        release_lock("192.168.1.1", "2001", dir_str);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_lock_file_content() {
        let dir = temp_lock_dir();
        let dir_str = dir.to_str().unwrap();
        assert!(acquire_lock("10.0.0.1", "3000", dir_str).is_none());

        let lock_key = format!("{:x}", fnv1a_hash("10.0.0.1:3000"))[..8].to_string();
        let lock_path = dir.join(format!("{lock_key}.lock"));
        assert!(lock_path.exists());

        let content = std::fs::read_to_string(&lock_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], std::process::id().to_string()); // PID
        assert_eq!(lines[1], "10.0.0.1:3000"); // host:port
        // lines[2] is hostname, lines[3] is timestamp

        release_lock("10.0.0.1", "3000", dir_str);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_zombie_lock_cleanup() {
        let dir = temp_lock_dir();
        let dir_str = dir.to_str().unwrap();
        let lock_key = format!("{:x}", fnv1a_hash("10.0.0.2:4000"))[..8].to_string();
        let lock_path = dir.join(format!("{lock_key}.lock"));

        // Create zombie lock with invalid PID
        std::fs::write(&lock_path, "999999\n10.0.0.2:4000\n2020-01-01T00:00:00").unwrap();
        assert!(lock_path.exists());

        // acquire_lock should clean up zombie and succeed
        assert!(acquire_lock("10.0.0.2", "4000", dir_str).is_none());

        release_lock("10.0.0.2", "4000", dir_str);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_same_host_port_conflict() {
        let dir = temp_lock_dir();
        let dir_str = dir.to_str().unwrap();

        // First acquire succeeds
        assert!(acquire_lock("10.0.0.3", "5000", dir_str).is_none());

        // Second acquire should return our own PID (conflict)
        let result = acquire_lock("10.0.0.3", "5000", dir_str);
        assert_eq!(result, Some(std::process::id()));

        release_lock("10.0.0.3", "5000", dir_str);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_release_and_reacquire() {
        let dir = temp_lock_dir();
        let dir_str = dir.to_str().unwrap();

        // Acquire
        assert!(acquire_lock("10.0.0.4", "6000", dir_str).is_none());

        // Release
        release_lock("10.0.0.4", "6000", dir_str);

        // Re-acquire should succeed
        assert!(acquire_lock("10.0.0.4", "6000", dir_str).is_none());

        release_lock("10.0.0.4", "6000", dir_str);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_different_hosts_no_conflict() {
        let dir = temp_lock_dir();
        let dir_str = dir.to_str().unwrap();

        assert!(acquire_lock("10.0.0.1", "2000", dir_str).is_none());
        assert!(acquire_lock("10.0.0.2", "2000", dir_str).is_none());

        release_lock("10.0.0.1", "2000", dir_str);
        release_lock("10.0.0.2", "2000", dir_str);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_fnv1a_hash_deterministic() {
        let h1 = fnv1a_hash("test:1234");
        let h2 = fnv1a_hash("test:1234");
        assert_eq!(h1, h2);

        let h3 = fnv1a_hash("test:1235");
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_process_alive() {
        // Current process should be alive
        assert!(process_alive(std::process::id()));

        // Very high PID should not be alive
        assert!(!process_alive(999999999));
    }

    #[test]
    fn test_acquire_lock_respects_custom_dir() {
        // Regression: the old recursive code changed lock_dir on retry. Verify
        // the lock is created in the specified directory.
        let dir = temp_lock_dir();
        let dir_str = dir.to_str().unwrap();
        assert!(acquire_lock("10.0.0.5", "7000", dir_str).is_none());

        // The lock file must be in `dir`, not in /tmp/debug-console/locks.
        let lock_key = format!("{:x}", fnv1a_hash("10.0.0.5:7000"))[..8].to_string();
        let lock_path = dir.join(format!("{lock_key}.lock"));
        assert!(
            lock_path.exists(),
            "lock should be in custom dir, not default"
        );

        release_lock("10.0.0.5", "7000", dir_str);
        std::fs::remove_dir_all(&dir).ok();
    }
}
