//! 网卡与文件系统过滤规则。

const IFACE_EXCLUDE_PREFIXES: [&str; 7] =
    ["docker", "veth", "br-", "virbr", "tun", "cni", "flannel"];

/// 该网卡的流量是否计入汇总（排除回环、容器与虚拟网卡）。
pub fn is_counted_iface(name: &str) -> bool {
    name != "lo"
        && !name.starts_with("kube")
        && !IFACE_EXCLUDE_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// 网卡是否计入：`include` 非空时只计入列表内的网卡（覆盖默认排除规则），否则用默认规则。
pub fn iface_selected(name: &str, include: &[String]) -> bool {
    if include.is_empty() {
        is_counted_iface(name)
    } else {
        include.iter().any(|i| i == name)
    }
}

/// 该文件系统类型的分区是否计入磁盘汇总。
pub fn is_counted_fs(fs_type: &str) -> bool {
    !matches!(
        fs_type,
        "tmpfs" | "devtmpfs" | "overlay" | "squashfs" | "proc" | "sysfs" | "nsfs"
    ) && !fs_type.starts_with("cgroup")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iface_filter() {
        for n in [
            "lo",
            "docker0",
            "veth12ab",
            "br-1f2e",
            "virbr0",
            "tun0",
            "cni0",
            "flannel.1",
            "kube-ipvs0",
        ] {
            assert!(!is_counted_iface(n), "{n}");
        }
        for n in ["eth0", "ens3", "enp1s0", "wlan0", "en0"] {
            assert!(is_counted_iface(n), "{n}");
        }
    }

    #[test]
    fn iface_include_overrides_default() {
        let inc = vec!["docker0".to_string(), "eth0".to_string()];
        assert!(iface_selected("docker0", &inc));
        assert!(iface_selected("eth0", &inc));
        assert!(!iface_selected("eth1", &inc));
        assert!(iface_selected("eth1", &[]));
        assert!(!iface_selected("lo", &[]));
    }

    #[test]
    fn fs_filter() {
        for f in [
            "tmpfs", "devtmpfs", "overlay", "squashfs", "proc", "sysfs", "nsfs", "cgroup",
            "cgroup2",
        ] {
            assert!(!is_counted_fs(f));
        }
        for f in ["ext4", "xfs", "btrfs", "apfs"] {
            assert!(is_counted_fs(f));
        }
    }
}
