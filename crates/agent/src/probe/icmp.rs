//! ICMP echo 探测：优先免特权 `SOCK_DGRAM`，失败再退回 `SOCK_RAW`。
//!
//! 全程只用套接字收发，不调用任何外部命令（如 `ping`）。
//!
//! 平台说明：
//! - Linux DGRAM ICMP：内核会把 echo 的 identifier 改写为套接字端口，因此 DGRAM 模式不校验
//!   identifier，改为按「序号 + 每轮随机 token（载荷前 8 字节）」匹配回包。
//! - IPv4 RAW 套接字（以及 macOS 的 DGRAM）收到的数据带 IP 头，解析时按 IHL 剥离。
//! - 校验和：IPv4 由我们按 RFC 1071 计算（DGRAM 下内核也会重算）；ICMPv6 校验和依赖伪首部，
//!   DGRAM 与 RAW 的 ICMPv6 套接字均由内核计算，所以我们发送时填 0。
//! - 无法建立任何套接字时返回 `sent=0` 的「ICMP 不可用」，并缓存 10 分钟，期间不再尝试。

use std::time::Duration;

use super::{ERR_ICMP, ProbeOutcome};

/// 「ICMP 不可用」结果的缓存时长。
const UNAVAILABLE_TTL: Duration = Duration::from_secs(10 * 60);
/// 样本发送间隔。
const SAMPLE_GAP: Duration = Duration::from_millis(200);

/// 建套接字阶段的失败分类。
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenErr {
    /// DGRAM 与 RAW 都因权限/协议不支持而失败（值为 errno），按地址族缓存。
    Unavailable(i32),
    /// 目标不可达（connect 的 ENETUNREACH 等），只影响本次目标，不缓存。
    Unreachable,
    /// 其它瞬时错误（EMFILE、ENOBUFS 等），本轮失败，不缓存。
    Transient,
}

#[cfg(unix)]
type Sock = (tokio::io::unix::AsyncFd<socket2::Socket>, bool);
#[cfg(unix)]
type Opener = fn(std::net::IpAddr) -> Result<Sock, OpenErr>;

/// 记录 ICMP 是否不可用（按 IPv4/IPv6 分别缓存），避免每轮都重试建套接字。
pub struct IcmpState {
    /// 下标 0 = IPv4，1 = IPv6
    down_since: std::sync::Mutex<[Option<std::time::Instant>; 2]>,
    attempts: std::sync::atomic::AtomicU32,
    clock: fn() -> std::time::Instant,
    #[cfg(unix)]
    opener: Opener,
}

impl Default for IcmpState {
    fn default() -> Self {
        Self::new()
    }
}

impl IcmpState {
    pub fn new() -> Self {
        Self::build(
            std::time::Instant::now,
            #[cfg(unix)]
            open,
        )
    }

    fn build(clock: fn() -> std::time::Instant, #[cfg(unix)] opener: Opener) -> Self {
        Self {
            down_since: std::sync::Mutex::new([None, None]),
            attempts: std::sync::atomic::AtomicU32::new(0),
            clock,
            #[cfg(unix)]
            opener,
        }
    }

    /// 可注入时钟（测试用）。
    #[cfg(test)]
    pub(crate) fn with_clock(clock: fn() -> std::time::Instant) -> Self {
        Self::build(
            clock,
            #[cfg(unix)]
            open,
        )
    }

    /// 可注入建套接字函数（测试用）。
    #[cfg(all(test, unix))]
    fn with_opener(opener: Opener) -> Self {
        Self::build(std::time::Instant::now, opener)
    }

    /// 标记该地址族不可用（从当前时钟起算 10 分钟）。
    /// 返回是否为新标记（此前未标记或已过期），用于只告警一次。
    pub(crate) fn mark_unavailable(&self, v6: bool) -> bool {
        let mut g = self.down_since.lock().unwrap_or_else(|e| e.into_inner());
        let now = (self.clock)();
        let fresh = !matches!(g[usize::from(v6)],
            Some(t) if now.saturating_duration_since(t) < UNAVAILABLE_TTL);
        g[usize::from(v6)] = Some(now);
        fresh
    }

    /// 该地址族是否仍处于不可用缓存期内；过期则清除标记。
    pub(crate) fn is_unavailable(&self, v6: bool) -> bool {
        let mut g = self.down_since.lock().unwrap_or_else(|e| e.into_inner());
        let slot = &mut g[usize::from(v6)];
        match *slot {
            Some(t) if (self.clock)().saturating_duration_since(t) < UNAVAILABLE_TTL => true,
            Some(_) => {
                *slot = None;
                false
            }
            None => false,
        }
    }

    /// 已进行的建套接字尝试次数。
    #[cfg(test)]
    pub(crate) fn attempts(&self) -> u32 {
        self.attempts.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(unix)]
    fn note_attempt(&self) {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

fn unavailable() -> ProbeOutcome {
    ProbeOutcome {
        sent: 0,
        lost: 0,
        avg_ms: None,
        min_ms: None,
        max_ms: None,
        error: Some(ERR_ICMP),
    }
}

/// RFC 1071 互联网校验和。
fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let (pairs, rest) = data.as_chunks::<2>();
    for c in pairs {
        sum += u32::from(u16::from_be_bytes(*c));
    }
    if let [last] = rest {
        sum += u32::from(*last) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn build_echo_typed(ty: u8, id: u16, seq: u16, payload: &[u8], with_checksum: bool) -> Vec<u8> {
    let mut p = Vec::with_capacity(8 + payload.len());
    p.extend_from_slice(&[ty, 0, 0, 0]);
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(payload);
    if with_checksum {
        let c = checksum(&p).to_be_bytes();
        p[2] = c[0];
        p[3] = c[1];
    }
    p
}

/// 构造 ICMPv4 echo request（含校验和）。
#[cfg(test)]
fn build_echo(id: u16, seq: u16, payload: &[u8]) -> Vec<u8> {
    build_echo_typed(8, id, seq, payload, true)
}

/// 解析回包，返回 `(seq, 载荷)`。IPv4 数据若带 IP 头则按 IHL 剥离；
/// `dgram` 为真时不校验 identifier（Linux 内核会改写）。
fn parse_reply(buf: &[u8], v6: bool, expect_id: u16, dgram: bool) -> Option<(u16, &[u8])> {
    let icmp = match buf.first() {
        Some(b) if !v6 && b >> 4 == 4 => buf.get(usize::from(b & 0x0f) * 4..)?,
        _ => buf,
    };
    if icmp.len() < 8 || icmp[0] != if v6 { 129 } else { 0 } || icmp[1] != 0 {
        return None;
    }
    if !dgram && u16::from_be_bytes([icmp[4], icmp[5]]) != expect_id {
        return None;
    }
    Some((u16::from_be_bytes([icmp[6], icmp[7]]), &icmp[8..]))
}

/// 解析 ICMPv4 echo reply，返回序号。
#[cfg(test)]
fn parse_echo_reply(buf: &[u8], expect_id: u16, dgram: bool) -> Option<u16> {
    parse_reply(buf, false, expect_id, dgram).map(|(s, _)| s)
}

/// 探测 `target`（主机名或 IP，不带端口）。
pub async fn probe_icmp(
    target: &str,
    samples: u32,
    timeout: Duration,
    state: &IcmpState,
) -> ProbeOutcome {
    let Some(ip) = resolve_ip(target, timeout).await else {
        return super::resolve_failed(samples);
    };
    if state.is_unavailable(ip.is_ipv6()) {
        return unavailable();
    }
    run(ip, samples, timeout, state).await
}

/// 解析一次，IPv4 优先。
async fn resolve_ip(target: &str, timeout: Duration) -> Option<std::net::IpAddr> {
    if let Ok(ip) = target.parse() {
        return Some(ip);
    }
    let addrs: Vec<_> =
        match tokio::time::timeout(timeout, tokio::net::lookup_host((target, 0))).await {
            Ok(Ok(it)) => it.collect(),
            _ => return None,
        };
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or(addrs.first())
        .map(|a| a.ip())
}

#[cfg(not(unix))]
async fn run(
    _ip: std::net::IpAddr,
    _samples: u32,
    _timeout: Duration,
    state: &IcmpState,
) -> ProbeOutcome {
    state.mark_unavailable(_ip.is_ipv6());
    unavailable()
}

#[cfg(unix)]
async fn run(
    ip: std::net::IpAddr,
    samples: u32,
    timeout: Duration,
    state: &IcmpState,
) -> ProbeOutcome {
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    use super::{Fail, finish};

    state.note_attempt();
    let v6 = ip.is_ipv6();
    let (sock, dgram) = match (state.opener)(ip) {
        Ok(s) => s,
        Err(OpenErr::Unavailable(errno)) => {
            if state.mark_unavailable(v6) {
                // 不记录目标地址
                tracing::warn!(
                    family = if v6 { "IPv6" } else { "IPv4" },
                    errno,
                    "ICMP 不可用，10 分钟内不再重试；请检查 net.ipv4.ping_group_range 或授予 CAP_NET_RAW"
                );
            }
            return unavailable();
        }
        Err(OpenErr::Unreachable) => {
            return finish(&vec![None; samples as usize], &[Fail::Connect]);
        }
        Err(OpenErr::Transient) => {
            return finish(&vec![None; samples as usize], &[Fail::Other]);
        }
    };
    let id: u16 = rand::random();
    let token: u64 = rand::random();
    let mut results = Vec::with_capacity(samples as usize);
    let mut fails = Vec::new();
    let mut last_send: Option<Instant> = None;
    for i in 0..samples {
        if let Some(t) = last_send {
            tokio::time::sleep_until((t + SAMPLE_GAP).into()).await;
        }
        let seq = i as u16;
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        let mut payload = Vec::with_capacity(16);
        payload.extend_from_slice(&token.to_be_bytes());
        payload.extend_from_slice(&micros.to_be_bytes());
        let pkt = build_echo_typed(if v6 { 128 } else { 8 }, id, seq, &payload, !v6);
        let t0 = Instant::now();
        last_send = Some(t0);
        if let Err(e) = send(&sock, &pkt).await {
            results.push(None);
            fails.push(if is_unreachable(&e) {
                Fail::Connect
            } else {
                Fail::Other
            });
            continue;
        }
        // 连接套接字上 recv 报告的 ICMP 错误（如主机不可达）
        let mut unreach_seen = false;
        let wait = async {
            let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 1500];
            let mut errors = 0u32;
            loop {
                let n = match recv(&sock, &mut buf).await {
                    Ok(n) => n,
                    Err(e) => {
                        // 无关错误不终止本样本，继续等到超时；异常持续出现则放弃
                        unreach_seen |= is_unreachable(&e);
                        errors += 1;
                        if errors > 100 {
                            return Err(e);
                        }
                        continue;
                    }
                };
                // SAFETY: recv 已初始化前 n 字节
                let data: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast(), n) };
                if let Some((s, body)) = parse_reply(data, v6, id, dgram)
                    && s == seq
                    && body.starts_with(&token.to_be_bytes())
                {
                    return Ok::<_, std::io::Error>(t0.elapsed().as_secs_f64() * 1000.0);
                }
            }
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(Ok(ms)) => results.push(Some(ms)),
            Ok(Err(_)) => {
                results.push(None);
                fails.push(Fail::Other);
            }
            Err(_) => {
                results.push(None);
                fails.push(if unreach_seen {
                    Fail::Connect
                } else {
                    Fail::Timeout
                });
            }
        }
    }
    finish(&results, &fails)
}

/// 不可达类错误：只影响当前目标。
#[cfg(unix)]
fn is_unreachable(e: &std::io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(
            libc::ENETUNREACH
                | libc::EHOSTUNREACH
                | libc::EHOSTDOWN
                | libc::ENETDOWN
                | libc::EADDRNOTAVAIL
                | libc::ECONNREFUSED
        )
    )
}

/// 建套接字失败是否属于「此环境不允许/不支持 ICMP」。
#[cfg(unix)]
fn is_permanent_open_error(e: &std::io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(
            libc::EPERM
                | libc::EACCES
                | libc::EPROTONOSUPPORT
                | libc::EAFNOSUPPORT
                | libc::ESOCKTNOSUPPORT
        )
    )
}

/// 建套接字：DGRAM 优先，权限/协议类失败再退回 RAW；已 connect 到目标并设为非阻塞。
/// 返回 `(套接字, 是否 DGRAM)`。
#[cfg(unix)]
fn open(ip: std::net::IpAddr) -> Result<Sock, OpenErr> {
    use socket2::{Domain, Protocol, SockAddr, Socket, Type};
    let (domain, proto) = if ip.is_ipv4() {
        (Domain::IPV4, Protocol::ICMPV4)
    } else {
        (Domain::IPV6, Protocol::ICMPV6)
    };
    let classify = |e: &std::io::Error| {
        if is_permanent_open_error(e) {
            OpenErr::Unavailable(e.raw_os_error().unwrap_or(0))
        } else {
            OpenErr::Transient
        }
    };
    let (sock, dgram) = match Socket::new(domain, Type::DGRAM, Some(proto)) {
        Ok(s) => (s, true),
        Err(e) if is_permanent_open_error(&e) => (
            Socket::new(domain, Type::RAW, Some(proto)).map_err(|e| classify(&e))?,
            false,
        ),
        Err(_) => return Err(OpenErr::Transient),
    };
    sock.set_nonblocking(true).map_err(|_| OpenErr::Transient)?;
    sock.connect(&SockAddr::from(std::net::SocketAddr::new(ip, 0)))
        .map_err(|e| {
            if is_unreachable(&e) || e.raw_os_error() == Some(libc::EAFNOSUPPORT) {
                OpenErr::Unreachable
            } else {
                OpenErr::Transient
            }
        })?;
    // SAFETY: AsyncFd 独占持有该套接字，期间不会被别处关闭；已设为非阻塞。
    let fd = unsafe { tokio::io::unix::AsyncFd::register(sock) }.map_err(|_| OpenErr::Transient)?;
    Ok((fd, dgram))
}

#[cfg(unix)]
async fn send(fd: &tokio::io::unix::AsyncFd<socket2::Socket>, pkt: &[u8]) -> std::io::Result<()> {
    loop {
        let mut g = fd.writable().await?;
        match g.try_io(|s| s.get_ref().send(pkt)) {
            Ok(r) => return r.map(|_| ()),
            Err(_would_block) => continue,
        }
    }
}

#[cfg(unix)]
async fn recv(
    fd: &tokio::io::unix::AsyncFd<socket2::Socket>,
    buf: &mut [std::mem::MaybeUninit<u8>],
) -> std::io::Result<usize> {
    loop {
        let mut g = fd.readable().await?;
        match g.try_io(|s| s.get_ref().recv(buf)) {
            Ok(r) => return r,
            Err(_would_block) => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    #[test]
    fn echo_checksum_valid() {
        let p = build_echo(0x1234, 7, b"hello-minimon!!"); // 奇数长度载荷
        assert_eq!((p[0], p[1]), (8, 0));
        assert_eq!(checksum(&p), 0, "含校验和的报文按 RFC 1071 求和应为 0");
        let p = build_echo(1, 2, &[0xff; 16]);
        assert_eq!(checksum(&p), 0);
    }

    fn reply(ty: u8, id: u16, seq: u16) -> Vec<u8> {
        let mut p = build_echo(id, seq, b"tokentok");
        p[0] = ty;
        p
    }

    #[test]
    fn parse_reply_matches_seq() {
        assert_eq!(parse_echo_reply(&reply(0, 9, 3), 9, false), Some(3));
        assert_eq!(parse_echo_reply(&reply(8, 9, 3), 9, false), None);
        assert_eq!(parse_echo_reply(&reply(0, 10, 3), 9, false), None);
        // DGRAM 不校验 identifier
        assert_eq!(parse_echo_reply(&reply(0, 10, 3), 9, true), Some(3));
        assert_eq!(parse_echo_reply(&[0, 0, 0], 9, true), None);
        // ICMPv6：type 129
        assert_eq!(
            parse_reply(&reply(129, 9, 4), true, 9, false).map(|r| r.0),
            Some(4)
        );
        assert_eq!(parse_reply(&reply(0, 9, 4), true, 9, false), None);
    }

    #[test]
    fn parse_reply_strips_ip_header() {
        let mut pkt = vec![
            0x45, 0, 0, 0, 0, 0, 0, 0, 64, 1, 0, 0, 127, 0, 0, 1, 127, 0, 0, 1,
        ];
        pkt.extend(reply(0, 9, 5));
        assert_eq!(parse_echo_reply(&pkt, 9, false), Some(5));
        // 带 IP 选项（IHL=6）
        let mut pkt = vec![
            0x46, 0, 0, 0, 0, 0, 0, 0, 64, 1, 0, 0, 127, 0, 0, 1, 127, 0, 0, 1,
        ];
        pkt.extend([0; 4]);
        pkt.extend(reply(0, 9, 6));
        assert_eq!(parse_echo_reply(&pkt, 9, false), Some(6));
    }

    #[tokio::test]
    async fn icmp_localhost_or_unavailable() {
        let st = IcmpState::new();
        let o = probe_icmp("127.0.0.1", 3, Duration::from_secs(2), &st).await;
        if o.error == Some("ICMP 不可用") {
            assert_eq!((o.sent, o.lost), (0, 0));
            assert!(st.is_unavailable(false));
        } else {
            assert_eq!((o.sent, o.lost, o.error), (3, 0, None), "{o:?}");
            assert!(o.avg_ms.is_some());
        }
        assert!(!(o.sent > 0 && o.lost == o.sent), "不得出现伪 100% 丢包");
    }

    static OFFSET: AtomicU64 = AtomicU64::new(0);
    fn fake_clock() -> Instant {
        static BASE: OnceLock<Instant> = OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_secs(OFFSET.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn unavailable_is_cached_for_10_minutes() {
        let st = IcmpState::with_clock(fake_clock);
        st.mark_unavailable(false);
        for off in [0, 60, 9 * 60] {
            OFFSET.store(off, Ordering::SeqCst);
            let o = probe_icmp("127.0.0.1", 5, Duration::from_millis(500), &st).await;
            assert_eq!((o.sent, o.lost, o.error), (0, 0, Some("ICMP 不可用")));
            assert_eq!(st.attempts(), 0, "缓存期内不得尝试建套接字 (t={off}s)");
        }
        OFFSET.store(10 * 60 + 1, Ordering::SeqCst);
        let _ = probe_icmp("127.0.0.1", 1, Duration::from_millis(500), &st).await;
        assert_eq!(st.attempts(), 1, "过期后应重新尝试");
    }

    #[cfg(unix)]
    fn unreachable_opener(_: std::net::IpAddr) -> Result<Sock, OpenErr> {
        Err(OpenErr::Unreachable)
    }
    #[cfg(unix)]
    fn transient_opener(_: std::net::IpAddr) -> Result<Sock, OpenErr> {
        Err(OpenErr::Transient)
    }
    #[cfg(unix)]
    fn denied_opener(_: std::net::IpAddr) -> Result<Sock, OpenErr> {
        Err(OpenErr::Unavailable(1))
    }

    #[tokio::test]
    async fn v6_unavailable_does_not_block_v4() {
        let st = IcmpState::new();
        assert!(st.mark_unavailable(true));
        assert!(
            !st.mark_unavailable(true),
            "缓存期内重复标记不再算新标记（不重复告警）"
        );
        let o = probe_icmp("::1", 2, Duration::from_millis(500), &st).await;
        assert_eq!((o.sent, o.error), (0, Some("ICMP 不可用")));
        assert_eq!(st.attempts(), 0, "v6 缓存期内不得尝试");
        let _ = probe_icmp("127.0.0.1", 1, Duration::from_millis(500), &st).await;
        assert_eq!(st.attempts(), 1, "v4 不受 v6 缓存影响");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unreachable_is_connect_failure_not_cached() {
        let st = IcmpState::with_opener(unreachable_opener);
        let o = probe_icmp("::1", 3, Duration::from_millis(500), &st).await;
        assert_eq!((o.sent, o.lost, o.error), (3, 3, Some("连接失败")));
        assert!(!st.is_unavailable(true) && !st.is_unavailable(false));
        // 下一轮仍会尝试
        let _ = probe_icmp("::1", 1, Duration::from_millis(500), &st).await;
        assert_eq!(st.attempts(), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transient_open_error_is_probe_failure_not_cached() {
        let st = IcmpState::with_opener(transient_opener);
        let o = probe_icmp("127.0.0.1", 2, Duration::from_millis(500), &st).await;
        assert_eq!((o.sent, o.lost, o.error), (2, 2, Some("探测失败")));
        assert!(!st.is_unavailable(false));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn permission_error_caches_only_its_family() {
        let st = IcmpState::with_opener(denied_opener);
        let o = probe_icmp("127.0.0.1", 2, Duration::from_millis(500), &st).await;
        assert_eq!((o.sent, o.error), (0, Some("ICMP 不可用")));
        assert!(st.is_unavailable(false) && !st.is_unavailable(true));
    }

    #[cfg(unix)]
    #[test]
    fn errno_classification() {
        let e = |n| std::io::Error::from_raw_os_error(n);
        assert!(is_unreachable(&e(libc::ENETUNREACH)));
        assert!(is_unreachable(&e(libc::EHOSTUNREACH)));
        assert!(!is_unreachable(&e(libc::EMFILE)));
        for n in [
            libc::EPERM,
            libc::EACCES,
            libc::EPROTONOSUPPORT,
            libc::EAFNOSUPPORT,
        ] {
            assert!(is_permanent_open_error(&e(n)));
        }
        assert!(!is_permanent_open_error(&e(libc::EMFILE)));
        assert!(!is_permanent_open_error(&e(libc::ENOBUFS)));
    }
}
