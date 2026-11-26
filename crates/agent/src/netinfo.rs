//! 公网地址与国家：GET Cloudflare `/cdn-cgi/trace`（纯文本 `key=value` 行），只出站。

use std::time::Duration;

use minimon_common::protocol::NetInfo;

pub const V4_URL: &str = "https://1.1.1.1/cdn-cgi/trace";
pub const V6_URL: &str = "https://[2606:4700:4700::1111]/cdn-cgi/trace";
/// 两次检测的间隔（每次连上 server 也会立即检测一次）。
pub const PERIOD: Duration = Duration::from_secs(6 * 3600);
const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_BODY: usize = 4096;

/// 解析 trace 正文，返回 `(ip, loc)`；`loc` 只接受 2 位字母，且不是 `XX`（未知）/ `T1`（Tor）。
pub fn parse_trace(body: &str) -> (Option<String>, Option<String>) {
    let (mut ip, mut loc) = (None, None);
    for line in body.lines() {
        let Some((k, v)) = line.trim().split_once('=') else {
            continue;
        };
        let v = v.trim();
        match k {
            "ip" if !v.is_empty() => ip = Some(v.to_string()),
            "loc" if v.len() == 2 && v.bytes().all(|b| b.is_ascii_alphabetic()) => {
                let c = v.to_ascii_uppercase();
                if c != "XX" {
                    loc = Some(c);
                }
            }
            _ => {}
        }
    }
    (ip, loc)
}

/// 并发查询 v4 / v6 两个地址；失败的一侧为 None。
pub async fn detect(v4_url: &str, v6_url: &str) -> NetInfo {
    let fetch = |url: &str| {
        let url = url.to_string();
        async move {
            crate::probe::http::http_get_body(&url, TIMEOUT, MAX_BODY)
                .await
                .map(|b| parse_trace(&b))
                .unwrap_or((None, None))
        }
    };
    let ((ip4, loc4), (ip6, loc6)) = tokio::join!(fetch(v4_url), fetch(v6_url));
    let family = |ip: Option<String>, v4: bool| {
        ip.filter(|s| match s.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(_)) => v4,
            Ok(std::net::IpAddr::V6(_)) => !v4,
            Err(_) => false,
        })
    };
    NetInfo {
        ipv4: family(ip4, true),
        ipv6: family(ip6, false),
        country: loc4.or(loc6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_trace_cases() {
        assert_eq!(
            parse_trace("fl=1\nip=203.0.113.5\nts=1\nloc=JP\n"),
            (Some("203.0.113.5".into()), Some("JP".into()))
        );
        for loc in ["XX", "T1", "jpn", ""] {
            let body = format!("ip=1.2.3.4\nloc={loc}\n");
            assert_eq!(parse_trace(&body).1, None, "{loc}");
        }
        assert_eq!(parse_trace("ip=1.2.3.4\n").1, None);
        assert_eq!(parse_trace(""), (None, None));
        assert_eq!(parse_trace("ip=\r\nloc=us\r\n"), (None, Some("US".into())));
    }

    /// 访问真实的 Cloudflare（需外网）：`cargo test -p minimon-agent live_cloudflare -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn live_cloudflare() {
        let n = detect(V4_URL, V6_URL).await;
        println!("{n:?}");
        assert!(n.ipv4.is_some() || n.ipv6.is_some(), "{n:?}");
        assert!(n.country.is_some(), "{n:?}");
    }
}
