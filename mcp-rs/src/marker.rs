//! Marker generation — 仿 labgrid util/marker.py
//!
//! 生成 10 字符随机 marker，排除 R/I/D 以避免与 ERROR/FAIL/INFO/DEBUG
//! 等日志关键字冲突。

use rand::Rng;

/// 可用字符池: A-Z 排除 R, I, D
const MARKER_POOL: &[u8] = b"ABCEFGHJKLMNOPQSTUVWXYZ";

/// 生成 10 字符随机大写字母 marker
pub fn gen_marker() -> String {
    let mut rng = rand::thread_rng();
    (0..10)
        .map(|_| {
            let idx = rng.gen_range(0..MARKER_POOL.len());
            MARKER_POOL[idx] as char
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn marker_length() {
        assert_eq!(gen_marker().len(), 10);
    }

    #[test]
    fn marker_chars() {
        let m = gen_marker();
        assert!(m.chars().all(|c| c.is_ascii_uppercase()));
        assert!(!m.contains('R'));
        assert!(!m.contains('I'));
        assert!(!m.contains('D'));
    }

    #[test]
    fn marker_unique() {
        let a = gen_marker();
        let b = gen_marker();
        assert_ne!(a, b);
    }

    #[test]
    fn test_marker_many_unique() {
        let mut markers = HashSet::new();
        for _ in 0..1000 {
            let m = gen_marker();
            assert!(markers.insert(m), "Duplicate marker generated");
        }
        assert_eq!(markers.len(), 1000);
    }

    #[test]
    fn test_marker_all_from_pool() {
        for _ in 0..100 {
            let m = gen_marker();
            for c in m.chars() {
                assert!(MARKER_POOL.contains(&(c as u8)), "Char {} not in pool", c);
            }
        }
    }

    #[test]
    fn test_marker_pool_size() {
        // A-Z = 26, minus R,I,D = 23
        assert_eq!(MARKER_POOL.len(), 23);
    }

    #[test]
    fn test_marker_excluded_chars() {
        // Verify R, I, D are not in pool
        assert!(!MARKER_POOL.contains(&b'R'));
        assert!(!MARKER_POOL.contains(&b'I'));
        assert!(!MARKER_POOL.contains(&b'D'));
    }
}
