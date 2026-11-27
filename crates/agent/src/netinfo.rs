//! 公网地址与国家：GET Cloudflare `/cdn-cgi/trace`（纯文本 `key=value` 行），只出站。

use std::time::Duration;

use minimon_common::protocol::NetInfo;

use crate::probe::http::{IpFamily, http_get_body};

/// 依次尝试：IP 直连免 DNS；部分网络（如大陆、个别海外机房）屏蔽 1.1.1.1，退回域名。
pub const V4_URLS: [&str; 2] = [
    "https://1.1.1.1/cdn-cgi/trace",
    "https://www.cloudflare.com/cdn-cgi/trace",
];
pub const V6_URLS: [&str; 2] = [
    "https://[2606:4700:4700::1111]/cdn-cgi/trace",
    "https://www.cloudflare.com/cdn-cgi/trace",
];
/// 两次检测的间隔（每次连上 server 也会立即检测一次）。
pub const PERIOD: Duration = Duration::from_secs(6 * 3600);
/// 一个地址都没查到时的重试间隔（不等 6 小时）。
pub const RETRY: Duration = Duration::from_secs(5 * 60);
/// 重连后复用上次结果的时限：超过才重新查询（动态 IP 断线重连后仍能较快更新）。
pub const MIN_GAP: Duration = Duration::from_secs(10 * 60);
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

/// 各地址族按顺序尝试的 trace 地址。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetinfoUrls {
    pub v4: Vec<String>,
    pub v6: Vec<String>,
}

impl NetinfoUrls {
    pub fn defaults() -> Self {
        Self {
            v4: V4_URLS.iter().map(|s| s.to_string()).collect(),
            v6: V6_URLS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// 按顺序尝试，返回第一个拿到对应地址族 IP 的 (ip, 国家)。
async fn first_hit(urls: &[String], family: IpFamily) -> (Option<String>, Option<String>) {
    for url in urls {
        let Some(body) = http_get_body(url, Some(family), TIMEOUT, MAX_BODY).await else {
            continue;
        };
        let (ip, loc) = parse_trace(&body);
        let ok = ip
            .as_deref()
            .and_then(|s| s.parse::<std::net::IpAddr>().ok())
            .is_some_and(|a| match family {
                IpFamily::V4 => a.is_ipv4(),
                IpFamily::V6 => a.is_ipv6(),
            });
        if ok {
            return (ip, loc);
        }
    }
    (None, None)
}

/// 两个地址族并发查询；任一查询失败只影响该族，国家优先取 IPv4 那次的结果。
pub async fn detect(urls: &NetinfoUrls) -> NetInfo {
    let ((ipv4, loc4), (ipv6, loc6)) = tokio::join!(
        first_hit(&urls.v4, IpFamily::V4),
        first_hit(&urls.v6, IpFamily::V6)
    );
    NetInfo {
        ipv4,
        ipv6,
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
    /// 本地 HTTP mock（只监听 127.0.0.1）：`/ok` 返回 trace 正文，其余路径 404。
    async fn mock() -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let ok = buf[..n].starts_with(b"GET /ok ");
                    let resp = if ok {
                        "HTTP/1.1 200 OK\r\nContent-Length: 27\r\n\r\nfl=1\nip=203.0.113.5\nloc=NG\n"
                    } else {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"
                    };
                    let _ = s.write_all(resp.as_bytes()).await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn detect_falls_back_to_next_url() {
        let port = mock().await;
        let urls = NetinfoUrls {
            v4: vec![
                format!("http://127.0.0.1:{port}/blocked"),
                format!("http://127.0.0.1:{port}/ok"),
            ],
            v6: vec![],
        };
        let n = detect(&urls).await;
        assert_eq!(n.ipv4.as_deref(), Some("203.0.113.5"));
        assert_eq!(n.country.as_deref(), Some("NG"));
        assert_eq!(n.ipv6, None);
    }

    /// 域名同时有 A 与 AAAA 时，v4 查询只连 IPv4 地址（mock 只监听 127.0.0.1，
    /// 若先连 ::1 会失败）；v6 查询不会连到 IPv4 地址。
    #[tokio::test]
    async fn detect_forces_address_family() {
        let port = mock().await;
        let url = format!("http://localhost:{port}/ok");
        let n = detect(&NetinfoUrls {
            v4: vec![url.clone()],
            v6: vec![url],
        })
        .await;
        assert_eq!(n.ipv4.as_deref(), Some("203.0.113.5"));
        assert_eq!(n.ipv6, None);
    }

    #[test]
    fn defaults_have_domain_fallback() {
        let d = NetinfoUrls::defaults();
        assert_eq!(d.v4.len(), 2);
        assert_eq!(d.v6.len(), 2);
        assert!(d.v4[1].contains("www.cloudflare.com"));
        assert!(d.v6[1].contains("www.cloudflare.com"));
    }

    #[tokio::test]
    #[ignore]
    async fn live_cloudflare() {
        let n = detect(&NetinfoUrls::defaults()).await;
        println!("{n:?}");
        assert!(n.ipv4.is_some() || n.ipv6.is_some(), "{n:?}");
        assert!(n.country.is_some(), "{n:?}");
    }
}
