//! 通知消息模板：均返回 `(title, body)`，时间按全局时区显示。

use jiff::Timestamp;

use super::metric::format_value;
use super::model::{AlertRule, Op};
use crate::period::Tz;

/// 毫秒时长 → 中文（「45 秒」「2 分钟」「3 小时 5 分钟」「2 天 4 小时」）。
pub fn duration(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    if secs < 60 {
        return format!("{secs} 秒");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins} 分钟");
    }
    let hours = mins / 60;
    if hours < 24 {
        return match mins % 60 {
            0 => format!("{hours} 小时"),
            m => format!("{hours} 小时 {m} 分钟"),
        };
    }
    match hours % 24 {
        0 => format!("{} 天", hours / 24),
        h => format!("{} 天 {h} 小时", hours / 24),
    }
}

/// UTC 毫秒 → 全局时区的 `YYYY-MM-DD HH:MM:SS`。
pub fn local_time(ms: i64, tz: &Tz) -> String {
    match Timestamp::from_millisecond(ms) {
        Ok(ts) => tz.to_datetime(ts).strftime("%Y-%m-%d %H:%M:%S").to_string(),
        Err(_) => ms.to_string(),
    }
}

/// 字节数 → 1024 进制的可读字符串。
pub fn bytes(b: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v.abs() >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

fn op_sym(op: Op) -> &'static str {
    match op {
        Op::Gt => ">",
        Op::Lt => "<",
    }
}

fn task_line(task: Option<&str>) -> String {
    task.map(|t| format!("探测任务：{t}\n")).unwrap_or_default()
}

pub fn firing(
    rule: &AlertRule,
    node: &str,
    task: Option<&str>,
    value: f64,
    since_ms: i64,
    tz: &Tz,
) -> (String, String) {
    let m = rule.metric;
    let title = format!("【告警】{node} {} {}", m.label(), format_value(m, value));
    let body = format!(
        "规则：{}\n{}条件：{} {} {}\n持续：{}\n开始时间：{}\n当前值：{}",
        rule.name,
        task_line(task),
        m.label(),
        op_sym(rule.op),
        format_value(m, rule.threshold),
        duration(rule.duration_secs.saturating_mul(1000)),
        local_time(since_ms, tz),
        format_value(m, value),
    );
    (title, body)
}

pub fn resolved(
    rule: &AlertRule,
    node: &str,
    task: Option<&str>,
    value: Option<f64>,
    lasted_ms: i64,
) -> (String, String) {
    let m = rule.metric;
    let title = format!("【恢复】{node} {}", m.label());
    let current = value.map_or_else(|| "无数据".to_string(), |v| format_value(m, v));
    let body = format!(
        "规则：{}\n{}告警持续了：{}\n当前值：{current}",
        rule.name,
        task_line(task),
        duration(lasted_ms),
    );
    (title, body)
}

pub fn offline(node: &str, since_ms: i64, tz: &Tz) -> (String, String) {
    (
        format!("【离线】{node}"),
        format!(
            "节点「{node}」已离线，自 {} 起没有上报。",
            local_time(since_ms, tz)
        ),
    )
}

pub fn online(node: &str, offline_for_ms: i64) -> (String, String) {
    (
        format!("【上线】{node}"),
        format!(
            "节点「{node}」已恢复上线，离线时长 {}。",
            duration(offline_for_ms)
        ),
    )
}

pub fn traffic(node: &str, level: i64, used: i64, quota: i64) -> (String, String) {
    let pct = if quota > 0 {
        used as f64 * 100.0 / quota as f64
    } else {
        0.0
    };
    (
        format!("【流量】{node} 已用 {level}%"),
        format!(
            "本账期已用 {} / 额度 {}（{pct:.1}%）。",
            bytes(used),
            bytes(quota)
        ),
    )
}

pub fn expire(node: &str, days: i64, date: &str) -> (String, String) {
    (
        format!("【到期】{node} 还剩 {days} 天"),
        format!("节点「{node}」将于 {date} 到期，还剩 {days} 天。"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(45_000), "45 秒");
        assert_eq!(duration(120_000), "2 分钟");
        assert_eq!(duration(3_600_000), "1 小时");
        assert_eq!(duration(3_900_000), "1 小时 5 分钟");
        assert_eq!(duration(86_400_000 * 2 + 3_600_000 * 4), "2 天 4 小时");
        assert_eq!(duration(-5), "0 秒");
    }

    #[test]
    fn byte_units() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.50 KB");
        assert_eq!(bytes(1 << 30), "1.00 GB");
    }

    #[test]
    fn local_time_uses_tz() {
        let tz = crate::period::parse_tz("Asia/Shanghai").unwrap();
        assert_eq!(local_time(0, &tz), "1970-01-01 08:00:00");
    }
}
