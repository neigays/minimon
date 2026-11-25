//! TCP/UDP 连接数统计（仅 Linux，读 /proc/net）。

/// 统计 /proc/net/* 内容中表头之外的非空行数。
pub fn count_lines(content: &str) -> u32 {
    content
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .count() as u32
}

#[cfg(target_os = "linux")]
fn count_files(paths: &[&str]) -> u32 {
    paths
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|c| count_lines(&c))
        .sum()
}

/// 返回 (tcp, udp) 连接数。非 Linux 平台返回 (0, 0)。
#[cfg(target_os = "linux")]
pub fn tcp_udp_counts() -> (u32, u32) {
    (
        count_files(&["/proc/net/tcp", "/proc/net/tcp6"]),
        count_files(&["/proc/net/udp", "/proc/net/udp6"]),
    )
}

#[cfg(not(target_os = "linux"))]
pub fn tcp_udp_counts() -> (u32, u32) {
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_lines_skips_header() {
        assert_eq!(
            count_lines(
                "  sl  local_address ...\n   0: 0100007F:0019 ...\n   1: 00000000:0016 ...\n"
            ),
            2
        );
        assert_eq!(count_lines("  sl  local_address\n"), 0);
    }

    #[test]
    fn count_lines_ignores_blank_lines() {
        assert_eq!(count_lines("hdr\n\n  \n 0: x\n"), 1);
    }
}
