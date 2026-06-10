//! Host:port 互斥锁 — O_EXCL 原子创建，僵尸清理。

use std::path::Path;

/// 检查项目级单例: 同一 project_dir/.dut-serial/mcp.pid 已有活跃进程则返回其 PID
pub fn check_project_singleton(project_dir: &Path, dut_dir: &str) -> Option<u32> {
    let pid_file = project_dir.join(dut_dir).join("mcp.pid");
    if !pid_file.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&pid_file).ok()?;
    let pid: u32 = content.trim().parse().ok()?;
    if process_alive(pid) {
        // 验证是 embedded-debug-mcp 进程 (不是其他进程重用了 PID)
        if let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) {
            if comm.contains("embedded-debug") {
                return Some(pid);
            }
        }
    }
    // 僵尸 PID 文件 → 清理
    std::fs::remove_file(&pid_file).ok();
    None
}

/// 获取 host:target 的锁 (target 可以是端口号或设备路径)。
/// 返回 `None` = 成功，`Some(pid)` = 冲突 PID。
pub fn acquire_lock(host: &str, target: &str, lock_dir: &str) -> Option<u32> {
    let lock_key = format!("{:x}", fnv1a_hash(&format!("{host}:{target}")))[..8].to_string();
    let lock_dir = Path::new(lock_dir);
    let lock_path = lock_dir.join(format!("{lock_key}.lock"));

    std::fs::create_dir_all(lock_dir).ok();

    // 检查已有锁
    if lock_path.exists() {
        if let Some(conflicting_pid) = check_existing_lock(&lock_path) {
            return Some(conflicting_pid);
        }
        // 僵尸锁 → 清理
        std::fs::remove_file(&lock_path).ok();
    }

    // O_EXCL 原子创建
    match try_create_lock(&lock_path, host, target) {
        Ok(()) => None,
        Err(_) => {
            // 竞态: 另一个进程抢先创建
            if let Some(pid) = check_existing_lock(&lock_path) {
                Some(pid)
            } else {
                std::fs::remove_file(&lock_path).ok();
                acquire_lock(host, target, lock_dir.to_str().unwrap_or("/tmp/embedded-debug/locks"))
            }
        }
    }
}

/// 释放 host:target 锁
pub fn release_lock(host: &str, target: &str, lock_dir: &str) {
    let lock_key = format!("{:x}", fnv1a_hash(&format!("{host}:{target}")))[..8].to_string();
    let lock_path = Path::new(lock_dir).join(format!("{lock_key}.lock"));
    std::fs::remove_file(lock_path).ok();
}

fn check_existing_lock(lock_path: &Path) -> Option<u32> {
    let content = std::fs::read_to_string(lock_path).ok()?;
    let pid_str = content.lines().next()?;
    let pid: u32 = pid_str.trim().parse().ok()?;
    if process_alive(pid) {
        Some(pid)
    } else {
        None
    }
}

// Minimal libc kill binding — avoid nix dependency for one syscall.
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

fn process_alive(pid: u32) -> bool {
    // kill(pid, 0) 检查进程是否存在
    // SAFETY: kill(2) with sig=0 is a standard process existence check on Unix
    let ret = unsafe { kill(pid as i32, 0) };
    ret == 0
}

fn try_create_lock(lock_path: &Path, host: &str, target: &str) -> Result<(), ()> {
    use std::os::unix::fs::OpenOptionsExt;
    let pid = std::process::id();
    let timestamp = chrono::Local::now().to_rfc3339();
    let content = format!("{pid}\n{host}:{target}\n{timestamp}");

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(lock_path)
        .map_err(|_| ())?;

    use std::io::Write;
    let mut file = file;
    file.write_all(content.as_bytes()).map_err(|_| ())?;
    Ok(())
}

/// FNV-1a 64-bit hash — fast non-cryptographic hash for lock key uniqueness.
/// host:port space is small (hundreds), collision probability is negligible.
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
        let dir = std::env::temp_dir().join(format!(
            "embedded-debug-test-{}-{}", std::process::id(), id
        ));
        // 清理旧目录
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
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], std::process::id().to_string()); // PID
        assert_eq!(lines[1], "10.0.0.1:3000"); // host:port
        // lines[2] is timestamp

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
}
