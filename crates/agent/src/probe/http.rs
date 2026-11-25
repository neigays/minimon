//! HTTP/HTTPS 探测：手写最小 HTTP/1.1 GET，收到状态行即关闭，不跟随重定向。

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, crypto::ring};

use super::{Fail, ProbeOutcome, finish, resolve, resolve_failed};

/// 状态行的最大读取长度，超出仍无 `\r\n` 视为异常响应。
const MAX_STATUS_LINE: usize = 1024;

struct Url {
    https: bool,
    host: String,
    port: u16,
    host_header: String,
    path: String,
}

fn parse_url(url: &str) -> Option<Url> {
    // 片段不会发给服务端
    let url = url.split('#').next().unwrap_or(url);
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else {
        (false, url.strip_prefix("http://")?)
    };
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_owned()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_owned()),
    };
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let default: u16 = if https { 443 } else { 80 };
    let (host, port) = if let Some(r) = authority.strip_prefix('[') {
        let (h, tail) = r.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if tail.is_empty() => default,
            None => return None,
        };
        (h.to_owned(), port)
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        (h.to_owned(), p.parse().ok()?)
    } else {
        (authority.to_owned(), default)
    };
    if host.is_empty() || port == 0 {
        return None;
    }
    // Host 头原样使用 authority（含方括号与显式端口）
    let host_header = authority.to_owned();
    Some(Url {
        https,
        host,
        port,
        host_header,
        path,
    })
}

fn tls_config() -> Option<Arc<ClientConfig>> {
    static CFG: OnceLock<Option<Arc<ClientConfig>>> = OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let cfg = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .ok()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Some(Arc::new(cfg))
    })
    .clone()
}

/// 发送请求并读到状态行；返回（状态码, 从发出请求到收到首字节的毫秒数）。
async fn request<S>(s: &mut S, u: &Url) -> Result<(u16, f64), Fail>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: minimon-agent\r\nConnection: close\r\n\r\n",
        u.path, u.host_header
    );
    let t0 = Instant::now();
    s.write_all(req.as_bytes())
        .await
        .map_err(|_| Fail::Connect)?;
    s.flush().await.map_err(|_| Fail::Connect)?;
    let mut buf = Vec::with_capacity(128);
    let mut chunk = [0u8; 256];
    let mut first_byte_ms = None;
    loop {
        let n = s.read(&mut chunk).await.map_err(|_| Fail::Connect)?;
        if n == 0 {
            return Err(Fail::Other);
        }
        first_byte_ms.get_or_insert_with(|| t0.elapsed().as_secs_f64() * 1000.0);
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(2).any(|w| w == b"\r\n") || buf.len() > MAX_STATUS_LINE {
            break;
        }
    }
    let line_end = buf
        .windows(2)
        .position(|w| w == b"\r\n")
        .ok_or(Fail::Other)?;
    let line = std::str::from_utf8(&buf[..line_end]).map_err(|_| Fail::Other)?;
    let mut parts = line.split(' ');
    let version_ok = parts.next().is_some_and(|v| v.starts_with("HTTP/1."));
    let code: u16 = parts
        .next()
        .and_then(|c| c.parse().ok())
        .filter(|_| version_ok)
        .ok_or(Fail::Other)?;
    Ok((code, first_byte_ms.unwrap_or(0.0)))
}

async fn sample(
    u: &Url,
    addr: std::net::SocketAddr,
    tls: Option<&TlsConnector>,
) -> Result<f64, Fail> {
    let tcp = TcpStream::connect(addr).await.map_err(|_| Fail::Connect)?;
    let (code, ms) = match tls {
        None => {
            let mut tcp = tcp;
            request(&mut tcp, u).await?
        }
        Some(c) => {
            let name = ServerName::try_from(u.host.clone()).map_err(|_| Fail::Other)?;
            let mut s = c.connect(name, tcp).await.map_err(|_| Fail::Connect)?;
            request(&mut s, u).await?
        }
    };
    // 连接随 drop 关闭，不读 body
    if (200..400).contains(&code) {
        Ok(ms)
    } else {
        Err(Fail::Status)
    }
}

/// GET `url`；2xx/3xx 成功，其它状态计丢失。`limit` 为每个样本的总超时。
pub async fn probe_http(url: &str, samples: u32, limit: Duration) -> ProbeOutcome {
    let Some(u) = parse_url(url) else {
        return resolve_failed(samples);
    };
    let connector = if u.https {
        match tls_config() {
            Some(c) => Some(TlsConnector::from(c)),
            None => return finish(&vec![None; samples as usize], &[Fail::Other]),
        }
    } else {
        None
    };
    let hostport = if u.host.contains(':') {
        format!("[{}]:{}", u.host, u.port)
    } else {
        format!("{}:{}", u.host, u.port)
    };
    let Some(addr) = resolve(&hostport, limit).await else {
        return resolve_failed(samples);
    };
    let mut results = Vec::with_capacity(samples as usize);
    let mut fails = Vec::new();
    for _ in 0..samples {
        match timeout(limit, sample(&u, addr, connector.as_ref())).await {
            Ok(Ok(ms)) => results.push(Some(ms)),
            Ok(Err(f)) => {
                results.push(None);
                fails.push(f);
            }
            Err(_) => {
                results.push(None);
                fails.push(Fail::Timeout);
            }
        }
    }
    finish(&results, &fails)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let u = parse_url("http://example.com").unwrap();
        assert_eq!(
            (u.https, u.host.as_str(), u.port, u.path.as_str()),
            (false, "example.com", 80, "/")
        );
        assert_eq!(u.host_header, "example.com");
        let u = parse_url("https://example.com:8443/a/b?q=1").unwrap();
        assert_eq!((u.https, u.port, u.path.as_str()), (true, 8443, "/a/b?q=1"));
        assert_eq!(u.host_header, "example.com:8443");
        let u = parse_url("http://example.com?x=1").unwrap();
        assert_eq!(u.path, "/?x=1");
        let u = parse_url("http://example.com/a?q=1#frag").unwrap();
        assert_eq!(u.path, "/a?q=1");
        let u = parse_url("http://example.com#frag").unwrap();
        assert_eq!((u.host.as_str(), u.path.as_str()), ("example.com", "/"));
    }

    #[test]
    fn parse_ipv6_and_invalid() {
        let u = parse_url("http://[::1]:8080/").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("::1", 8080));
        let u = parse_url("https://[::1]/").unwrap();
        assert_eq!(
            (u.host.as_str(), u.port, u.host_header.as_str()),
            ("::1", 443, "[::1]")
        );
        for bad in [
            "ftp://x/",
            "example.com",
            "http://",
            "http://h:0/",
            "http://h:x/",
            "http://u@h/",
        ] {
            assert!(parse_url(bad).is_none(), "{bad}");
        }
    }
}
