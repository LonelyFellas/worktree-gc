//! 卷可用空间。
//!
//! 存在的理由是 **du 的估算不可信**：APFS 的写时复制会让同一批物理块被重复计数，
//! 而 pnpm v10 在 macOS 上默认就走 clonefile。报告里说「可回收 168GB」，
//! 实际执行完可能只多出 40GB —— 数字对不上就会丢掉用户的信任。
//!
//! 所以执行前后各读一次这里，报**实测差值**，而把 du 的结果标成"约"。

use crate::model::Cause;
use std::path::Path;

/// 该路径所在卷的可用字节数。
pub fn available_bytes(path: &Path) -> Result<u64, Cause> {
    fs4::available_space(path).map_err(|e| Cause::Io {
        path: path.to_path_buf(),
        msg: format!("读取可用空间失败: {e}"),
    })
}

/// 人类可读的体积。刻意用 1024 进制并保留一位小数，与 `du -h` 对得上，
/// 便于用户拿工具的输出和自己跑的 du 相互印证。
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n}B") } else { format!("{v:.1}{}", UNITS[i]) }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn reports_available_space_for_tmp() {
        let n = available_bytes(std::env::temp_dir().as_path()).expect("应能读到可用空间");
        assert!(n > 0, "临时目录所在卷不该是 0 可用");
    }

    #[test]
    fn missing_path_is_an_error_not_zero() {
        // 返回 0 会被上层当成「盘满了」，必须是错误
        assert!(available_bytes(Path::new("/definitely/not/here/xyzzy")).is_err());
    }

    #[test]
    fn formats_like_du_h() {
        assert_eq!(human_bytes(0), "0B");
        assert_eq!(human_bytes(512), "512B");
        assert_eq!(human_bytes(1024), "1.0K");
        assert_eq!(human_bytes(40 * 1024 * 1024 * 1024), "40.0G");
    }
}
