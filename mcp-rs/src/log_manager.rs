//! Log manager — per-power-cycle 日志切割，存于 .dut-serial/logs/。
//!
//! 一次上电→掉电 = 一个 boot-NNN.log。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct LogManager {
    log_dir: PathBuf,
    dut_dir: PathBuf,
    max_logs: usize,
    max_file_size: u64,
    current_file: Option<fs::File>,
    current_path: Option<PathBuf>,
    /// serial.full.log: 全量连续日志 (从不截断)
    full_file: Option<fs::File>,
    boot_number: u32,
    /// 内存环形缓冲 — 板子运行时数据暂存于此, 检测到启动后落盘
    pub ring_buffer: Vec<u8>,
    /// 当前启动序列在 ring_buffer 中的起始位置
    boot_start_pos: usize,
    /// ring_buffer 最大容量 (默认 2MB)
    max_buffer_size: usize,
}

impl LogManager {
    pub fn new(project_dir: &Path, max_logs: usize, max_file_size_mb: u64, dut_dir: &str) -> Self {
        let dut_dir_path = project_dir.join(dut_dir);
        let log_dir = dut_dir_path.join("logs");

        Self {
            log_dir,
            dut_dir: dut_dir_path,
            max_logs,
            max_file_size: max_file_size_mb * 1024 * 1024,
            current_file: None,
            current_path: None,
            full_file: None,
            boot_number: 0,
            ring_buffer: Vec::new(),
            boot_start_pos: 0,
            max_buffer_size: 2 * 1024 * 1024, // 2MB
        }
    }

    pub fn current_path(&self) -> Option<&Path> {
        self.current_path.as_deref()
    }

    pub fn boot_number(&self) -> u32 {
        self.boot_number
    }

    fn next_boot_number(&mut self) -> u32 {
        let count_file = self.dut_dir.join(".boot_count");
        let n = fs::read_to_string(&count_file)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0) + 1;
        fs::write(&count_file, n.to_string()).ok();
        self.boot_number = n;
        n
    }

    /// 打开当前周期日志文件
    pub fn open_current(&mut self) {
        let n = self.next_boot_number();
        fs::create_dir_all(&self.log_dir).ok();

        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let fname = format!("boot-{:03}_{ts}.log", n);
        let path = self.log_dir.join(&fname);

        match fs::OpenOptions::new().append(true).create(true).open(&path) {
            Ok(mut file) => {
                let header = format!(
                    "=== Boot #{} — {} ===\n",
                    n,
                    chrono::Local::now().to_rfc3339()
                );
                file.write_all(header.as_bytes()).ok();
                self.current_file = Some(file);
                self.current_path = Some(path);

                // 更新符号链接 serial.current.log → 当前日志
                let link = self.log_dir.join("serial.current.log");
                std::fs::remove_file(&link).ok();
                #[cfg(unix)]
                std::os::unix::fs::symlink(&fname, &link).ok();
            }
            Err(e) => {
                tracing::error!("Failed to open log file {path:?}: {e}");
            }
        }
    }

    /// 追加原始串口数据: 写入 serial.current.log + serial.full.log + 内存缓冲
    pub fn write(&mut self, data: &[u8]) {
        if self.current_file.is_none() {
            self.ensure_current_file();
        }
        let clean = strip_ansi_and_null(data);
        // serial.current.log (当前启动周期)
        if let Some(ref mut file) = self.current_file {
            if !clean.is_empty() {
                file.write_all(&clean).ok();
            }
        }
        // serial.full.log (全量连续, 从不截断)
        if let Some(ref mut file) = self.full_file {
            if !clean.is_empty() {
                file.write_all(&clean).ok();
            }
        }
        // 内存环形缓冲
        self.ring_buffer.extend_from_slice(&clean);
        if self.ring_buffer.len() > self.max_buffer_size {
            let drain = self.ring_buffer.len() - self.max_buffer_size / 2;
            self.ring_buffer.drain(..drain);
            self.boot_start_pos = self.boot_start_pos.saturating_sub(drain);
        }
    }

    /// 确保 serial.current.log + serial.full.log 已打开
    pub fn ensure_current_file(&mut self) {
        fs::create_dir_all(&self.log_dir).ok();
        // serial.current.log (当前启动周期, 可截断)
        if self.current_file.is_none() {
            let path = self.log_dir.join("serial.current.log");
            if path.is_symlink() { std::fs::remove_file(&path).ok(); }
            match fs::OpenOptions::new().create(true).append(true).open(&path) {
                Ok(file) => { self.current_file = Some(file); self.current_path = Some(path); }
                Err(e) => tracing::error!("Failed to open serial.current.log: {e}"),
            }
        }
        // serial.full.log (全量连续, 从不截断)
        if self.full_file.is_none() {
            let full = self.log_dir.join("serial.full.log");
            match fs::OpenOptions::new().create(true).append(true).open(&full) {
                Ok(file) => { self.full_file = Some(file); }
                Err(e) => tracing::error!("Failed to open serial.full.log: {e}"),
            }
        }
    }

    /// 标记新启动周期: 保存快照 → 截断 serial.current.log → 重置缓冲
    pub fn mark_boot_start(&mut self) {
        self.flush_boot_log();
        self.current_file.take();
        let current = self.log_dir.join("serial.current.log");
        if current.exists() { std::fs::write(&current, b"").ok(); }
        self.ensure_current_file();
        self.cleanup_old_logs();
        self.boot_start_pos = self.ring_buffer.len();
    }

    /// 将缓冲区内容保存为新启动日志文件
    pub fn flush_boot_log(&mut self) {
        if self.boot_start_pos >= self.ring_buffer.len() {
            return; // 无新数据
        }
        let boot_data = self.ring_buffer[self.boot_start_pos..].to_vec();
        if boot_data.is_empty() {
            return;
        }
        // 创建新日志文件
        fs::create_dir_all(&self.log_dir).ok();
        let n = self.next_boot_number();
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let fname = format!("boot-{:03}_{ts}.log", n);
        let path = self.log_dir.join(&fname);

        if let Ok(mut file) = fs::OpenOptions::new().append(true).create(true).open(&path) {
            file.write_all(&boot_data).ok();
            self.current_path = Some(path);
            tracing::info!("Boot log saved: {} ({} bytes)", fname, boot_data.len());
        }
        // 清理旧日志
        self.cleanup_old_logs();
        // 重置起点
        self.boot_start_pos = self.ring_buffer.len();
    }

    /// 切割: flush buffer → 打开新日志 → 清理旧日志
    pub fn rotate(&mut self) {
        self.flush_boot_log();
    }

    pub fn close(&mut self) {
        self.current_file.take();
    }

    /// 列出所有归档启动日志 (最新在前)
    pub fn list_archives(&self) -> Vec<ArchiveInfo> {
        let mut logs = self.list_log_files();
        logs.sort_by(|a, b| b.path.cmp(&a.path));
        logs.into_iter()
            .enumerate()
            .map(|(i, p)| ArchiveInfo {
                index: i,
                filename: p.name,
                size_bytes: p.size,
                path: p.path,
            })
            .collect()
    }

    /// 读取指定归档日志
    pub fn read_log(&self, archive_index: usize, lines: usize, pattern: Option<&str>) -> LogContent {
        let logs = self.list_log_files_sorted();
        if archive_index >= logs.len() {
            return LogContent {
                content: String::new(),
                filename: String::new(),
                total_lines: 0,
                filtered_lines: 0,
            };
        }
        let target = &logs[archive_index];
        let content = fs::read_to_string(&target.path).unwrap_or_default();
        let all_lines: Vec<&str> = content.lines().collect();
        let total_lines = all_lines.len();

        let filtered: Vec<&str> = if let Some(pat) = pattern {
            if let Ok(re) = regex::RegexBuilder::new(pat)
                .case_insensitive(true)
                .build()
            {
                all_lines.into_iter().filter(|l| re.is_match(l)).collect()
            } else {
                all_lines
            }
        } else {
            all_lines
        };

        let filtered_lines = filtered.len();
        let result: Vec<&str> = if lines > 0 && filtered.len() > lines {
            filtered[filtered.len() - lines..].to_vec()
        } else {
            filtered
        };

        LogContent {
            content: result.join("\n"),
            filename: target.name.clone(),
            total_lines,
            filtered_lines,
        }
    }

    // ── internal ──

    fn cleanup_old_logs(&self) {
        let mut logs = self.list_log_files();
        // Sort by modification time, oldest first
        logs.sort_by(|a, b| {
            let ta = std::fs::metadata(&a.path).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            let tb = std::fs::metadata(&b.path).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            ta.cmp(&tb)
        });
        if logs.len() > self.max_logs {
            for old in &logs[..logs.len() - self.max_logs] {
                fs::remove_file(&old.path).ok();
                tracing::debug!("Cleaned old log: {}", old.name);
            }
        }
    }

    fn list_log_files(&self) -> Vec<LogFileInfo> {
        let mut result = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.log_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("log")
                    && !path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n == "serial.current.log")
                {
                    let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    result.push(LogFileInfo {
                        name: path.file_name().unwrap().to_string_lossy().to_string(),
                        size,
                        path,
                    });
                }
            }
        }
        result
    }

    fn list_log_files_sorted(&self) -> Vec<LogFileInfo> {
        let mut logs = self.list_log_files();
        logs.sort_by(|a, b| b.path.cmp(&a.path));
        logs
    }
}

/// 过滤 ANSI 转义码 + null 字节 + 其他控制字符（保留 \n \r \t）
fn strip_ansi_and_null(data: &[u8]) -> Vec<u8> {
    let re_ansi = regex::bytes::Regex::new(r"(\x1b\[|\x9b)[0-?]*[ -/]*[@-~]|\x1b[>=]|\x1b[()][A-Z0-9]").unwrap();
    let mut result = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if b == 0x00 {
            // 跳过 null 字节
            i += 1;
            continue;
        }
        if b == 0x1b || b == 0x9b {
            // ANSI 序列起始终，用 regex 跳过整段
            if let Some(m) = re_ansi.find(&data[i..]) {
                i += m.end();
                continue;
            }
        }
        // 保留可打印字符 + newline/cr/tab
        if b >= 0x20 || b == b'\n' || b == b'\r' || b == b'\t' {
            result.push(b);
        }
        i += 1;
    }
    result
}

struct LogFileInfo {
    name: String,
    #[allow(dead_code)]
    size: u64,
    path: PathBuf,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct ArchiveInfo {
    pub index: usize,
    pub filename: String,
    pub size_bytes: u64,
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct LogContent {
    pub content: String,
    pub filename: String,
    pub total_lines: usize,
    pub filtered_lines: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_log_manager(max_logs: usize, max_size_mb: u64) -> (LogManager, TempDir) {
        let tmp = TempDir::new().unwrap();
        let lm = LogManager::new(tmp.path(), max_logs, max_size_mb, ".dut-serial");
        (lm, tmp)
    }

    #[test]
    fn test_open_current_creates_file() {
        let (mut lm, tmp) = create_test_log_manager(10, 100);
        lm.open_current();

        assert_eq!(lm.boot_number(), 1);
        assert!(lm.current_path().is_some());
        assert!(lm.current_path().unwrap().exists());
        assert_eq!(lm.current_path().unwrap().extension().unwrap(), "log");

        let log_dir = tmp.path().join(".dut-serial/logs");
        assert!(log_dir.join("serial.current.log").exists());
    }

    #[test]
    fn test_boot_number_increments() {
        let (mut lm, _tmp) = create_test_log_manager(10, 100);

        lm.open_current();
        assert_eq!(lm.boot_number(), 1);

        lm.rotate();
        assert_eq!(lm.boot_number(), 2);

        lm.rotate();
        assert_eq!(lm.boot_number(), 3);
    }

    #[test]
    fn test_write_data() {
        let (mut lm, _tmp) = create_test_log_manager(10, 100);
        lm.open_current();

        lm.write(b"line 1\n");
        lm.write(b"line 2\n");
        lm.write(b"line 3\n");

        let content = fs::read_to_string(lm.current_path().unwrap()).unwrap();
        assert!(content.contains("line 1"));
        assert!(content.contains("line 2"));
        assert!(content.contains("line 3"));
    }

    #[test]
    fn test_rotate_creates_new_file() {
        let (mut lm, _tmp) = create_test_log_manager(10, 100);

        lm.open_current();
        let path1 = lm.current_path().unwrap().to_path_buf();
        lm.write(b"boot 1 data\n");

        lm.rotate();
        let path2 = lm.current_path().unwrap().to_path_buf();
        lm.write(b"boot 2 data\n");

        assert_ne!(path1, path2);
        assert!(path1.exists());
        assert!(path2.exists());

        let content1 = fs::read_to_string(&path1).unwrap();
        let content2 = fs::read_to_string(&path2).unwrap();
        assert!(content1.contains("boot 1 data"));
        assert!(content2.contains("boot 2 data"));
    }

    #[test]
    fn test_cleanup_old_logs() {
        let (mut lm, _tmp) = create_test_log_manager(3, 100); // keep only 3

        // Create first log
        lm.open_current();
        lm.write(b"boot 0\n");

        // Create 4 more logs via rotate (total 5)
        // Need 1+ second delays to avoid timestamp collisions in filenames
        for i in 1..5 {
            std::thread::sleep(std::time::Duration::from_secs(2));
            lm.rotate();
            lm.write(format!("boot {i}\n").as_bytes());
        }

        // Should keep at most max_logs
        let archives = lm.list_archives();
        assert!(archives.len() <= 3, "Expected <= 3 logs, got {}", archives.len());
    }

    #[test]
    fn test_list_archives_ordering() {
        let (mut lm, _tmp) = create_test_log_manager(10, 100);

        // Create 3 logs with different content
        for i in 1..=3 {
            lm.open_current();
            lm.write(format!("boot {i}\n").as_bytes());
            std::thread::sleep(std::time::Duration::from_millis(10)); // ensure different timestamps
        }

        let archives = lm.list_archives();
        assert_eq!(archives.len(), 3);

        // Should be ordered newest first (by path, which includes timestamp)
        assert!(archives[0].path > archives[1].path);
        assert!(archives[1].path > archives[2].path);
    }

    #[test]
    fn test_read_log_basic() {
        let (mut lm, _tmp) = create_test_log_manager(10, 100);
        lm.open_current();

        // Note: open_current writes a header line
        for i in 1..=10 {
            lm.write(format!("line {i}\n").as_bytes());
        }

        let result = lm.read_log(0, 5, None);
        // Total lines includes header
        assert!(result.total_lines >= 10);
        // filtered_lines counts all lines before the `lines` limit
        assert_eq!(result.filtered_lines, result.total_lines);
        // Content should have at most 5 lines (limited by `lines` parameter)
        let content_lines = result.content.lines().count();
        assert_eq!(content_lines, 5);
        assert!(result.content.contains("line 10"));
    }

    #[test]
    fn test_read_log_with_pattern() {
        let (mut lm, _tmp) = create_test_log_manager(10, 100);
        lm.open_current();

        lm.write(b"ERROR: something failed\n");
        lm.write(b"INFO: all good\n");
        lm.write(b"ERROR: another failure\n");
        lm.write(b"DEBUG: debugging\n");

        let result = lm.read_log(0, 100, Some("ERROR"));
        assert_eq!(result.filtered_lines, 2);
        assert!(result.content.contains("ERROR"));
        assert!(!result.content.contains("INFO"));
        assert!(!result.content.contains("DEBUG"));
    }

    #[test]
    fn test_read_log_archive_index() {
        let (mut lm, _tmp) = create_test_log_manager(10, 100);

        // Create boot 1
        lm.open_current();
        lm.write(b"boot 1 data\n");

        // Create boot 2
        lm.rotate();
        lm.write(b"boot 2 data\n");

        // Read boot 2 (index 0)
        let result = lm.read_log(0, 100, None);
        assert!(result.content.contains("boot 2 data"));

        // Read boot 1 (index 1)
        let result = lm.read_log(1, 100, None);
        assert!(result.content.contains("boot 1 data"));

        // Invalid index
        let result = lm.read_log(99, 100, None);
        assert_eq!(result.content, "");
    }

    #[test]
    fn test_symlink_updated() {
        let (mut lm, tmp) = create_test_log_manager(10, 100);

        lm.open_current();
        let link = tmp.path().join(".dut-serial/logs/serial.current.log");
        assert!(link.is_symlink());

        let target1 = fs::read_link(&link).unwrap();
        lm.rotate();
        let target2 = fs::read_link(&link).unwrap();

        assert_ne!(target1, target2);
    }

    #[test]
    fn test_boot_count_file() {
        let (mut lm, tmp) = create_test_log_manager(10, 100);
        let count_file = tmp.path().join(".dut-serial/.boot_count");

        lm.open_current();
        assert_eq!(fs::read_to_string(&count_file).unwrap(), "1");

        lm.rotate();
        assert_eq!(fs::read_to_string(&count_file).unwrap(), "2");
    }

    #[test]
    fn test_close() {
        let (mut lm, _tmp) = create_test_log_manager(10, 100);
        lm.open_current();
        assert!(lm.current_path().is_some());

        lm.close();
        // After close, current_path should still be Some (just file handle closed)
        assert!(lm.current_path().is_some());
    }

    #[test]
    fn test_log_file_header() {
        let (mut lm, _tmp) = create_test_log_manager(10, 100);
        lm.open_current();

        let content = fs::read_to_string(lm.current_path().unwrap()).unwrap();
        assert!(content.contains("=== Boot #1"));
        assert!(content.contains("==="));
    }

    #[test]
    fn test_max_file_size_rotation() {
        let (mut lm, _tmp) = create_test_log_manager(10, 0); // 0 MB = 0 bytes = rotate immediately

        lm.open_current();
        let path1 = lm.current_path().unwrap().to_path_buf();

        // Write some data (should trigger rotation due to 0 size limit)
        lm.write(b"some data\n");

        // Should have rotated
        let path2 = lm.current_path().unwrap().to_path_buf();
        assert_ne!(path1, path2);
    }
}
