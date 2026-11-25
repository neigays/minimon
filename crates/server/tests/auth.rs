mod common;

use common::{ADMIN_PASSWORD, cookie_pair, http, login, raw, start};

fn login_body(user: &str, pw: &str) -> String {
    serde_json::json!({"username": user, "password": pw}).to_string()
}

async fn do_login(port: u16, pw: &str) -> (u16, axum::http::HeaderMap, String) {
    http(
        port,
        "POST",
        "/api/auth/login",
        None,
        Some(&login_body("admin", pw)),
    )
    .await
}

fn err_of(body: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_else(|_| panic!("{body}"));
    v["error"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn login_sets_cookie_and_me_works() {
    let fx = start(|_| {}).await;
    let (code, h, body) = do_login(fx.port, ADMIN_PASSWORD).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body, r#"{"username":"admin"}"#);
    let sc = h.get("set-cookie").unwrap().to_str().unwrap();
    assert!(sc.starts_with("minimon_session="), "{sc}");
    for want in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=2592000"] {
        assert!(sc.contains(want), "{sc}");
    }
    assert!(!sc.contains("Secure"), "{sc}");
    let cookie = cookie_pair(&h).unwrap();
    let (code, _, body) = http(fx.port, "GET", "/api/auth/me", Some(&cookie), None).await;
    assert_eq!(code, 200);
    assert_eq!(body, r#"{"username":"admin"}"#);
    let (code, _, body) = http(fx.port, "GET", "/api/auth/me", None, None).await;
    assert_eq!(code, 401);
    assert!(err_of(&body).contains("登录"));
}

#[tokio::test]
async fn secure_cookie_only_behind_trusted_proxy() {
    let body = login_body("admin", ADMIN_PASSWORD);
    let fwd = [
        ("Content-Type", "application/json"),
        ("X-Forwarded-Proto", "https"),
    ];
    let fx = start(|_| {}).await;
    let (code, h, _) = raw(fx.port, "POST", "/api/auth/login", &fwd, Some(&body)).await;
    assert_eq!(code, 200);
    assert!(
        !h.get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Secure")
    );

    let fx = start(|c| c.real_ip_header = Some("X-Forwarded-For".into())).await;
    let (code, h, _) = raw(fx.port, "POST", "/api/auth/login", &fwd, Some(&body)).await;
    assert_eq!(code, 200);
    assert!(
        h.get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("; Secure")
    );
    // 配置了但请求不是 https：无 Secure
    let plain = [("Content-Type", "application/json")];
    let (_, h, _) = raw(fx.port, "POST", "/api/auth/login", &plain, Some(&body)).await;
    assert!(
        !h.get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Secure")
    );
}

#[tokio::test]
async fn wrong_password_then_429() {
    let fx = start(|_| {}).await;
    for _ in 0..5 {
        let (code, _, body) = do_login(fx.port, "nope-nope").await;
        assert_eq!(code, 401);
        assert_eq!(err_of(&body), "用户名或密码错误");
    }
    let (code, _, body) = do_login(fx.port, "nope-nope").await;
    assert_eq!(code, 429);
    err_of(&body);
    // 限流期间连正确密码也不放行
    let (code, _, _) = do_login(fx.port, ADMIN_PASSWORD).await;
    assert_eq!(code, 429);
}

#[tokio::test]
async fn wrong_username_same_error() {
    let fx = start(|_| {}).await;
    let (code, _, body) = http(
        fx.port,
        "POST",
        "/api/auth/login",
        None,
        Some(&login_body("root", ADMIN_PASSWORD)),
    )
    .await;
    assert_eq!(code, 401);
    assert_eq!(err_of(&body), "用户名或密码错误");
}

#[tokio::test]
async fn limiter_keyed_by_real_ip_when_configured() {
    let fx = start(|c| c.real_ip_header = Some("X-Forwarded-For".into())).await;
    let bad = login_body("admin", "nope-nope");
    let from = |ip: &'static str| {
        [
            ("Content-Type", "application/json"),
            ("X-Forwarded-For", ip),
        ]
    };
    // 客户端伪造的第一个地址不影响计数（取最后一个）
    for i in 0..5 {
        let spoof = format!("9.9.9.{i}, 7.7.7.7");
        let h = [
            ("Content-Type", "application/json"),
            ("X-Forwarded-For", spoof.as_str()),
        ];
        let (code, _, _) = raw(fx.port, "POST", "/api/auth/login", &h, Some(&bad)).await;
        assert_eq!(code, 401);
    }
    let (code, _, _) = raw(
        fx.port,
        "POST",
        "/api/auth/login",
        &from("7.7.7.7"),
        Some(&bad),
    )
    .await;
    assert_eq!(code, 429);
    let good = login_body("admin", ADMIN_PASSWORD);
    let (code, _, _) = raw(
        fx.port,
        "POST",
        "/api/auth/login",
        &from("8.8.8.8"),
        Some(&good),
    )
    .await;
    assert_eq!(code, 200);
}

#[tokio::test]
async fn logout_invalidates_session() {
    let fx = start(|_| {}).await;
    let cookie = login(fx.port).await;
    let (code, h, _) = http(
        fx.port,
        "POST",
        "/api/auth/logout",
        Some(&cookie),
        Some("{}"),
    )
    .await;
    assert_eq!(code, 200);
    let sc = h.get("set-cookie").unwrap().to_str().unwrap();
    assert!(sc.contains("Max-Age=0"), "{sc}");
    let (code, _, _) = http(fx.port, "GET", "/api/auth/me", Some(&cookie), None).await;
    assert_eq!(code, 401);
}

#[tokio::test]
async fn change_password_kills_other_sessions() {
    let fx = start(|_| {}).await;
    let a = login(fx.port).await;
    let b = login(fx.port).await;
    let body = r#"{"old_password":"test-password","new_password":"brand-new-pass"}"#;
    let (code, _, resp) = http(fx.port, "PUT", "/api/auth/password", Some(&a), Some(body)).await;
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        http(fx.port, "GET", "/api/auth/me", Some(&a), None).await.0,
        200
    );
    assert_eq!(
        http(fx.port, "GET", "/api/auth/me", Some(&b), None).await.0,
        401
    );
    assert_eq!(do_login(fx.port, "brand-new-pass").await.0, 200);
    assert_eq!(do_login(fx.port, ADMIN_PASSWORD).await.0, 401);
    // 未登录不能改密码
    let (code, _, _) = http(fx.port, "PUT", "/api/auth/password", None, Some(body)).await;
    assert_eq!(code, 401);
}

#[tokio::test]
async fn wrong_old_password_rejected() {
    let fx = start(|_| {}).await;
    let a = login(fx.port).await;
    let body = r#"{"old_password":"not-it-at-all","new_password":"brand-new-pass"}"#;
    let (code, _, resp) = http(fx.port, "PUT", "/api/auth/password", Some(&a), Some(body)).await;
    assert_eq!(code, 400, "{resp}");
    err_of(&resp);
    assert_eq!(do_login(fx.port, ADMIN_PASSWORD).await.0, 200);
}

#[tokio::test]
async fn password_edge_cases() {
    let fx = start(|_| {}).await;
    // 空密码登录 -> 400
    let (code, _, body) = do_login(fx.port, "").await;
    assert_eq!(code, 400);
    err_of(&body);
    // 超长密码登录 -> 401，不是 500
    let long = "x".repeat(10_000);
    assert_eq!(do_login(fx.port, &long).await.0, 401);

    let a = login(fx.port).await;
    let change = |new: &str, old: &str| {
        serde_json::json!({"old_password": old, "new_password": new}).to_string()
    };
    let put = |body: String, cookie: String| async move {
        http(
            fx.port,
            "PUT",
            "/api/auth/password",
            Some(&cookie),
            Some(&body),
        )
        .await
    };
    let (code, _, b) = put(change("1234567", ADMIN_PASSWORD), a.clone()).await;
    assert_eq!(code, 400);
    err_of(&b);
    let (code, _, b) = put(change(&"a".repeat(257), ADMIN_PASSWORD), a.clone()).await;
    assert_eq!(code, 400);
    err_of(&b);
    let (code, _, _) = put(change(&"a".repeat(256), ADMIN_PASSWORD), a.clone()).await;
    assert_eq!(code, 200);
    let emoji = "密码🔐abcdefgh";
    let (code, _, b) = put(change(emoji, &"a".repeat(256)), a.clone()).await;
    assert_eq!(code, 200, "{b}");
    assert_eq!(do_login(fx.port, emoji).await.0, 200);
}

#[tokio::test]
async fn non_json_mutation_rejected() {
    let fx = start(|_| {}).await;
    let body = login_body("admin", ADMIN_PASSWORD);
    let (code, _, resp) = raw(
        fx.port,
        "POST",
        "/api/auth/login",
        &[("Content-Type", "text/plain")],
        Some(&body),
    )
    .await;
    assert_eq!(code, 415);
    assert_eq!(err_of(&resp), "请求需为 JSON");
    let (code, _, _) = raw(fx.port, "POST", "/api/auth/login", &[], Some(&body)).await;
    assert_eq!(code, 415);
    // charset 参数与大小写不影响
    let (code, _, _) = raw(
        fx.port,
        "POST",
        "/api/auth/login",
        &[("Content-Type", "Application/JSON; charset=utf-8")],
        Some(&body),
    )
    .await;
    assert_eq!(code, 200);
}

#[tokio::test]
async fn malformed_json_is_json_error() {
    let fx = start(|_| {}).await;
    let (code, h, body) = http(fx.port, "POST", "/api/auth/login", None, Some("{not json")).await;
    assert_eq!(code, 400);
    assert!(
        h.get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("json")
    );
    err_of(&body);
    let (code, _, body) = http(
        fx.port,
        "POST",
        "/api/auth/login",
        None,
        Some(r#"{"username":"a"}"#),
    )
    .await;
    assert_eq!(code, 400);
    err_of(&body);
}
