//! 节点标签：`1Gbps<yellow>;CN2 GIA<red>`，`;` 分隔，可选 `<颜色>` 后缀（Radix 色板）。

use serde::Serialize;

/// 可用颜色（Radix Themes 彩色色板）；无后缀为 gray。
pub const COLORS: &[&str] = &[
    "gray", "gold", "bronze", "brown", "yellow", "amber", "orange", "tomato", "red", "ruby",
    "crimson", "pink", "plum", "purple", "violet", "iris", "indigo", "blue", "cyan", "teal",
    "jade", "green", "grass", "lime", "mint", "sky",
];
pub const MAX_TAGS: usize = 10;
pub const MAX_TEXT_CHARS: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Tag {
    pub text: String,
    pub color: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TagError {
    #[error("未知颜色 <{0}>，可用：{list}", list = COLORS.join(", "))]
    UnknownColor(String),
    #[error("最多 {MAX_TAGS} 个标签")]
    TooMany,
    #[error("标签「{0}」超过 {MAX_TEXT_CHARS} 个字符")]
    TooLong(String),
    #[error("标签不能包含 < > ;")]
    BadChar,
    #[error("标签文字不能为空")]
    Empty,
    #[error("颜色不能为空")]
    EmptyColor,
}

fn parse_one(seg: &str) -> Result<Tag, TagError> {
    let (text, color) = match seg.strip_suffix('>').and_then(|s| s.rsplit_once('<')) {
        Some((text, c)) => {
            if c.contains(['<', '>']) {
                return Err(TagError::BadChar);
            }
            let c = c.trim().to_ascii_lowercase();
            if c.is_empty() {
                return Err(TagError::EmptyColor);
            }
            let color = COLORS
                .iter()
                .copied()
                .find(|k| *k == c)
                .ok_or(TagError::UnknownColor(c))?;
            (text.trim(), color)
        }
        None => (seg, "gray"),
    };
    if text.contains(['<', '>', ';']) {
        return Err(TagError::BadChar);
    }
    if text.is_empty() {
        // 只有 `<颜色>` 没有文字
        return Err(TagError::Empty);
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(TagError::TooLong(text.to_string()));
    }
    Ok(Tag {
        text: text.to_string(),
        color,
    })
}

fn segments(raw: &str) -> impl Iterator<Item = &str> {
    raw.split(';').map(str::trim).filter(|s| !s.is_empty())
}

/// 严格解析：任何一段不合法都报错（保存时用）。
pub fn parse_tags(raw: &str) -> Result<Vec<Tag>, TagError> {
    let tags = segments(raw)
        .map(parse_one)
        .collect::<Result<Vec<_>, _>>()?;
    if tags.len() > MAX_TAGS {
        return Err(TagError::TooMany);
    }
    Ok(tags)
}

/// 规范串：`文字<颜色>` 用 `;` 连接，gray 无后缀的保持无后缀。
pub fn normalize_tags(raw: &str) -> Result<String, TagError> {
    parse_tags(raw)?;
    let parts: Vec<String> = segments(raw)
        .filter_map(|seg| {
            let t = parse_one(seg).ok()?;
            Some(if seg.ends_with('>') {
                format!("{}<{}>", t.text, t.color)
            } else {
                t.text
            })
        })
        .collect();
    Ok(parts.join(";"))
}

/// 展示用：容错，跳过坏段（存储值应已规范，异常时不报错）。
pub fn tags_for_view(stored: &str) -> Vec<Tag> {
    segments(stored)
        .filter_map(|s| parse_one(s).ok())
        .take(MAX_TAGS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(raw: &str) -> Vec<(String, &'static str)> {
        parse_tags(raw)
            .expect("ok")
            .into_iter()
            .map(|t| (t.text, t.color))
            .collect()
    }

    #[test]
    fn parse_basic() {
        assert_eq!(
            pairs("1Gbps<yellow>;CN2 GIA<red>"),
            vec![("1Gbps".into(), "yellow"), ("CN2 GIA".into(), "red")]
        );
        assert_eq!(pairs("BGP"), vec![("BGP".into(), "gray")]);
    }

    #[test]
    fn blank_segments_ignored() {
        assert_eq!(pairs(" ; ;"), vec![]);
        assert_eq!(normalize_tags(" ; ;").expect("ok"), "");
        assert_eq!(normalize_tags("").expect("ok"), "");
    }

    #[test]
    fn color_case_and_space() {
        assert_eq!(normalize_tags("1G< Red >").expect("ok"), "1G<red>");
    }

    #[test]
    fn unknown_color() {
        let e = parse_tags("x<rainbow>").expect_err("err").to_string();
        assert!(e.contains("rainbow") && e.contains("sky"), "{e}");
    }

    #[test]
    fn limits() {
        let eleven = ["a"; 11].join(";");
        assert_eq!(parse_tags(&eleven), Err(TagError::TooMany));
        assert_eq!(parse_tags(&["a"; 10].join(";")).expect("ok").len(), 10);
        assert!(matches!(
            parse_tags(&"x".repeat(25)),
            Err(TagError::TooLong(_))
        ));
        assert!(parse_tags(&"中".repeat(24)).is_ok());
        // 空颜色前的文字也要非空
        assert_eq!(parse_tags("<red>"), Err(TagError::Empty));
    }

    #[test]
    fn stray_angle_rejected() {
        assert_eq!(parse_tags("a<b<red>"), Err(TagError::BadChar));
        assert_eq!(parse_tags("a>b"), Err(TagError::BadChar));
        assert_eq!(parse_tags("a<red"), Err(TagError::BadChar));
    }

    #[test]
    fn empty_and_broken_color_messages() {
        assert_eq!(parse_tags("x<>"), Err(TagError::EmptyColor));
        assert_eq!(parse_tags("x< >"), Err(TagError::EmptyColor));
        assert_eq!(parse_tags("x<red>>"), Err(TagError::BadChar));
    }

    #[test]
    fn normalize_keeps_plain() {
        assert_eq!(
            normalize_tags(" BGP ; 1G<GREEN> ").expect("ok"),
            "BGP;1G<green>"
        );
    }

    #[test]
    fn view_is_tolerant() {
        assert_eq!(
            tags_for_view("ok<red>;bad<rainbow>;BGP"),
            vec![
                Tag {
                    text: "ok".into(),
                    color: "red"
                },
                Tag {
                    text: "BGP".into(),
                    color: "gray"
                }
            ]
        );
        assert_eq!(tags_for_view(""), vec![]);
    }
}
