//! 账期与到期日计算（纯函数）。时区来自内置 tzdb，不读宿主机时区。

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

pub type Tz = TimeZone;
pub const DEFAULT_TZ: &str = "Asia/Shanghai";

/// 解析 IANA 时区名；不在内置库中返回 None。
pub fn parse_tz(name: &str) -> Option<Tz> {
    TimeZone::get(name).ok()
}

/// `(year, month)` 的账期起点：当月第 `min(reset_day, 当月天数)` 日本地 00:00（UTC 毫秒）。
/// 遇到夏令时空洞/重复取兼容（较晚/较早）的瞬间。
fn start_of(year: i16, month: i8, reset_day: u8, tz: &Tz) -> Option<i64> {
    let first = Date::new(year, month, 1).ok()?;
    let day = (reset_day.clamp(1, 31) as i8).min(first.days_in_month());
    let dt = Date::new(year, month, day).ok()?.at(0, 0, 0, 0);
    Some(tz.to_timestamp(dt).ok()?.as_millisecond())
}

fn prev_month(year: i16, month: i8) -> (i16, i8) {
    if month == 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    }
}

fn next_month(year: i16, month: i8) -> (i16, i8) {
    if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    }
}

/// 本期账期 `[start, end)`（UTC 毫秒）。
///
/// 起点为本地日期中最近一个「当月第 `min(重置日, 当月天数)` 日 00:00」且 ≤ now；
/// 终点为下一期起点。时间戳超出库支持范围时退化为 `(now, now)`。
pub fn period_bounds(reset_day: u8, tz: &Tz, now_ms: i64) -> (i64, i64) {
    let Ok(ts) = Timestamp::from_millisecond(now_ms) else {
        return (now_ms, now_ms);
    };
    let local = tz.to_datetime(ts);
    let (y, m) = (local.year(), local.month());
    let (start_ym, end_ym) = match start_of(y, m, reset_day, tz) {
        Some(s) if s <= now_ms => ((y, m), next_month(y, m)),
        _ => (prev_month(y, m), (y, m)),
    };
    match (
        start_of(start_ym.0, start_ym.1, reset_day, tz),
        start_of(end_ym.0, end_ym.1, reset_day, tz),
    ) {
        (Some(s), Some(e)) => (s, e),
        _ => (now_ms, now_ms),
    }
}

/// 到期日 − 今天（按 `tz` 的本地日期）；日期非法返回 None。
pub fn expire_days(expire_date: &str, tz: &Tz, now_ms: i64) -> Option<i64> {
    // 只接受严格的 YYYY-MM-DD
    if expire_date.len() != 10 {
        return None;
    }
    let target: Date = expire_date.parse().ok()?;
    let today = tz
        .to_datetime(Timestamp::from_millisecond(now_ms).ok()?)
        .date();
    Some(today.duration_until(target).as_secs() / 86_400)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh() -> Tz {
        parse_tz("Asia/Shanghai").unwrap()
    }
    fn ms(s: &str) -> i64 {
        s.parse::<Timestamp>().unwrap().as_millisecond()
    }

    #[test]
    fn period_mid_month() {
        assert_eq!(
            period_bounds(5, &sh(), ms("2027-03-20T04:00:00Z")),
            (ms("2027-03-04T16:00:00Z"), ms("2027-04-04T16:00:00Z"))
        );
    }

    #[test]
    fn day31_in_february_uses_month_end() {
        assert_eq!(
            period_bounds(31, &sh(), ms("2027-02-28T17:00:00Z")).0,
            ms("2027-02-27T16:00:00Z")
        );
        assert_eq!(
            period_bounds(31, &sh(), ms("2028-02-29T17:00:00Z")).0,
            ms("2028-02-28T16:00:00Z")
        );
        assert_eq!(
            period_bounds(31, &sh(), ms("2027-03-15T00:00:00Z")).1,
            ms("2027-03-30T16:00:00Z")
        );
    }

    #[test]
    fn day31_before_month_end_belongs_to_previous_period() {
        // 2027-02-27 上海：2 月账期起点是 2-28，尚未到，仍属 1-31 起的账期
        let (s, e) = period_bounds(31, &sh(), ms("2027-02-27T04:00:00Z"));
        assert_eq!(s, ms("2027-01-30T16:00:00Z"));
        assert_eq!(e, ms("2027-02-27T16:00:00Z"));
    }

    #[test]
    fn reset_boundary_local_midnight() {
        assert_eq!(
            period_bounds(1, &sh(), ms("2027-03-31T15:59:59Z")).0,
            ms("2027-02-28T16:00:00Z")
        );
        assert_eq!(
            period_bounds(1, &sh(), ms("2027-03-31T16:00:00Z")).0,
            ms("2027-03-31T16:00:00Z")
        );
    }

    #[test]
    fn crosses_year() {
        assert_eq!(
            period_bounds(15, &sh(), ms("2027-01-03T00:00:00Z")).0,
            ms("2026-12-14T16:00:00Z")
        );
        assert_eq!(
            period_bounds(15, &sh(), ms("2026-12-20T00:00:00Z")).1,
            ms("2027-01-14T16:00:00Z")
        );
    }

    #[test]
    fn utc_differs_from_shanghai() {
        let now = ms("2027-03-20T04:00:00Z");
        let utc = parse_tz("UTC").unwrap();
        let (us, _) = period_bounds(5, &utc, now);
        let (ss, _) = period_bounds(5, &sh(), now);
        assert_eq!(us, ms("2027-03-05T00:00:00Z"));
        assert_eq!(us - ss, 8 * 3600 * 1000);
    }

    #[test]
    fn dst_zone_uses_local_midnight() {
        // 纽约 2027-03-14 02:00 起夏令时：3 月账期起点 03-01 00:00 EST = 05:00Z，
        // 下一期 04-01 00:00 EDT = 04:00Z
        let ny = parse_tz("America/New_York").unwrap();
        assert_eq!(
            period_bounds(1, &ny, ms("2027-03-20T12:00:00Z")),
            (ms("2027-03-01T05:00:00Z"), ms("2027-04-01T04:00:00Z"))
        );
    }

    #[test]
    fn bounds_are_contiguous_and_contain_now() {
        let now = ms("2027-08-09T10:11:12Z");
        for d in 1..=31u8 {
            let (s, e) = period_bounds(d, &sh(), now);
            assert!(s <= now && now < e, "day {d}");
            // 下一期起点正好是 e
            assert_eq!(period_bounds(d, &sh(), e).0, e, "day {d}");
        }
    }

    #[test]
    fn expire_days_cases() {
        assert_eq!(
            expire_days("2027-03-20", &sh(), ms("2027-03-10T00:00:00Z")),
            Some(10)
        );
        assert_eq!(
            expire_days("2027-03-10", &sh(), ms("2027-03-10T00:00:00Z")),
            Some(0)
        );
        assert_eq!(
            expire_days("2027-03-01", &sh(), ms("2027-03-10T00:00:00Z")),
            Some(-9)
        );
        assert_eq!(expire_days("2027-02-30", &sh(), 0), None);
        assert_eq!(expire_days("garbage", &sh(), 0), None);
    }

    #[test]
    fn expire_days_uses_local_date() {
        // UTC 2027-03-10 17:00 = 上海 03-11 01:00
        assert_eq!(
            expire_days("2027-03-11", &sh(), ms("2027-03-10T17:00:00Z")),
            Some(0)
        );
    }

    #[test]
    fn bad_tz_is_none() {
        assert!(parse_tz("Mars/Olympus").is_none());
        assert!(parse_tz("").is_none());
    }
}
