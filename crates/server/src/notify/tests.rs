use super::*;
use crate::alert::model::{EventKind, NewEvent, SendStatus};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;

fn notice(title: &str, body: &str) -> Notice {
    Notice {
        node_id: None,
        node_name: None,
        kind: EventKind::Firing,
        title: title.into(),
        body: body.into(),
    }
}

/// 极简 HTTP/1.1 mock：应答固定 JSON，并把每个请求（头+体）送入通道。
async fn mock(resp: &'static str) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/send/sctp1tkey.send", l.local_addr().unwrap());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(h) = text.find("\r\n\r\n") {
                        let len = text[..h]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if buf.len() >= h + 4 + len {
                            break;
                        }
                    }
                }
                let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
                let out = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    resp.len(),
                    resp
                );
                let _ = s.write_all(out.as_bytes()).await;
            });
        }
    });
    (url, rx)
}

fn form_value(req: &str, name: &str) -> String {
    let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
    for kv in body.split('&') {
        if let Some(v) = kv.strip_prefix(&format!("{name}=")) {
            let bytes = v.replace('+', " ");
            let b = bytes.as_bytes();
            let mut out = Vec::new();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' && i + 2 < b.len() {
                    let h = std::str::from_utf8(&b[i + 1..i + 3]).unwrap();
                    out.push(u8::from_str_radix(h, 16).unwrap());
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            return String::from_utf8(out).unwrap();
        }
    }
    String::new()
}

#[test]
fn parse_uid_cases() {
    assert_eq!(parse_uid("sctp123tAbC"), Some("123"));
    assert_eq!(parse_uid("SCT123"), None);
    assert_eq!(parse_uid("sctpxt"), None);
    assert_eq!(parse_uid(""), None);
}

#[test]
fn new_builds_client_without_global_provider() {
    assert!(ServerChan::new("sctp1tabc").is_ok());
}

#[test]
fn bad_sendkey_is_config_error() {
    let Err(e) = ServerChan::new("abcdef") else {
        panic!("应当失败");
    };
    assert!(matches!(e, NotifyError::Config(_)));
    assert!(!e.to_string().contains("abcdef"));
}

#[tokio::test]
async fn serverchan_posts_form_and_checks_code() {
    let (url, mut rx) = mock(r#"{"code":0}"#).await;
    let sc = ServerChan::with_endpoint(url).unwrap();
    sc.send("hi", "body").await.unwrap();
    let req = rx.recv().await.unwrap();
    assert!(req.starts_with("POST "));
    assert!(req.contains("title=") && req.contains("desp="));

    let (url, _rx) = mock(r#"{"code":40001,"message":"bad"}"#).await;
    let sc = ServerChan::with_endpoint(url).unwrap();
    match sc.send("hi", "b").await {
        Err(NotifyError::Api { code: 40001, .. }) => {}
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn title_truncated_to_32_chars() {
    let (url, mut rx) = mock(r#"{"code":0}"#).await;
    let sc = ServerChan::with_endpoint(url).unwrap();
    sc.send(&"汉".repeat(40), "b").await.unwrap();
    let req = rx.recv().await.unwrap();
    assert_eq!(form_value(&req, "title").chars().count(), 32);
}

#[tokio::test]
async fn http_error_hides_sendkey() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let sc = ServerChan::with_endpoint(format!("http://127.0.0.1:{port}/send/sctp9tSECRET.send"))
        .unwrap();
    let e = sc.send("t", "b").await.unwrap_err();
    assert!(!e.to_string().contains("SECRET"), "{e}");
    assert!(!format!("{e:?}").contains("SECRET"));
}

#[test]
fn merge_formats() {
    let one = notice("T", "B");
    assert_eq!(merge(std::slice::from_ref(&one)), ("T".into(), "B".into()));
    let three = vec![one.clone(), one.clone(), one.clone()];
    let (t, b) = merge(&three);
    assert_eq!(t, "minimon：3 条告警");
    assert!(b.contains("### T\nB"));
    let many: Vec<_> = (0..25).map(|_| notice("T", &"字".repeat(500))).collect();
    let (_, b) = merge(&many);
    assert!(b.chars().count() <= 4000);
    let small: Vec<_> = (0..25).map(|_| notice("T", "b")).collect();
    let (_, b) = merge(&small);
    assert!(b.contains("…另有 5 条"));
    assert_eq!(b.matches("### ").count(), 20);
}

struct Counting {
    calls: Arc<AtomicUsize>,
    titles: Arc<Mutex<Vec<String>>>,
    ok: bool,
}

impl Notifier for Counting {
    fn send<'a>(&'a self, title: &'a str, _b: &'a str) -> BoxFuture<'a, Result<(), NotifyError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.titles.lock().unwrap().push(title.to_string());
        let ok = self.ok;
        Box::pin(async move {
            if ok {
                Ok(())
            } else {
                Err(NotifyError::Http("boom".into()))
            }
        })
    }
}

struct Harness {
    db: Arc<Db>,
    calls: Arc<AtomicUsize>,
    titles: Arc<Mutex<Vec<String>>>,
    queue: NotifyQueue,
    _tx: watch::Sender<bool>,
}

fn harness(key: Option<&str>, ok: bool) -> Harness {
    let db = Arc::new(Db::open_in_memory().unwrap());
    if let Some(k) = key {
        db.set_sendkey(Some(k)).unwrap();
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let titles = Arc::new(Mutex::new(Vec::new()));
    let (c, t) = (calls.clone(), titles.clone());
    let make: MakeNotifier = Arc::new(move |_| {
        Ok(Box::new(Counting {
            calls: c.clone(),
            titles: t.clone(),
            ok,
        }) as Box<dyn Notifier>)
    });
    let (tx, rx) = watch::channel(false);
    let (queue, _h) = NotifyQueue::spawn(
        db.clone(),
        rx,
        Duration::from_secs(10),
        [Duration::from_secs(5), Duration::from_secs(30)],
        make,
    );
    Harness {
        db,
        calls,
        titles,
        queue,
        _tx: tx,
    }
}

fn add_event(db: &Db, title: &str) -> i64 {
    db.insert_alert_events(&[NewEvent {
        ts_ms: 1,
        node_id: None,
        node_name: None,
        kind: EventKind::Firing,
        title: title.into(),
        body: "b".into(),
        sent: SendStatus::Pending,
    }])
    .unwrap()[0]
}

async fn wait_settled(db: &Arc<Db>) -> Vec<SendStatus> {
    for _ in 0..200 {
        let d = db.clone();
        let evs = tokio::task::spawn_blocking(move || d.list_alert_events(None, 100).unwrap())
            .await
            .unwrap();
        if evs.iter().all(|e| e.sent != SendStatus::Pending) {
            return evs.iter().map(|e| e.sent).collect();
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("事件状态未落定");
}

#[tokio::test(start_paused = true)]
async fn queue_merges_within_window() {
    let h = harness(Some("sctp1tk"), true);
    let a = add_event(&h.db, "A");
    h.queue.enqueue(vec![a], vec![notice("A", "b")]);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let b = add_event(&h.db, "B");
    h.queue.enqueue(vec![b], vec![notice("B", "b")]);
    let st = wait_settled(&h.db).await;
    assert_eq!(st, vec![SendStatus::Ok, SendStatus::Ok]);
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    assert!(h.titles.lock().unwrap()[0].contains("2 条"));
}

#[tokio::test(start_paused = true)]
async fn queue_without_sendkey_marks_skipped() {
    let h = harness(None, true);
    let a = add_event(&h.db, "A");
    h.queue.enqueue(vec![a], vec![notice("A", "b")]);
    assert_eq!(wait_settled(&h.db).await, vec![SendStatus::Skipped]);
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn queue_retries_then_failed() {
    let h = harness(Some("sctp1tk"), false);
    let a = add_event(&h.db, "A");
    h.queue.enqueue(vec![a], vec![notice("A", "b")]);
    assert_eq!(wait_settled(&h.db).await, vec![SendStatus::Failed]);
    assert_eq!(h.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn queue_exits_on_shutdown_dropping_pending() {
    let db = Arc::new(Db::open_in_memory().unwrap());
    let make: MakeNotifier = Arc::new(|_| Err(NotifyError::Config("x")));
    let (tx, rx) = watch::channel(false);
    let (q, h) = NotifyQueue::spawn(
        db.clone(),
        rx,
        Duration::from_secs(10),
        [Duration::from_secs(5), Duration::from_secs(30)],
        make,
    );
    let a = add_event(&db, "A");
    q.enqueue(vec![a], vec![notice("A", "b")]);
    tokio::time::sleep(Duration::from_secs(1)).await;
    tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), h)
        .await
        .unwrap()
        .unwrap();
    let evs = db.list_alert_events(None, 10).unwrap();
    assert_eq!(evs[0].sent, SendStatus::Pending);
}
