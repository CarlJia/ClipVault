//! 自动类别启发式（R4 / AE1）。
//!
//! 把一段纯文本剪贴板内容归入 [`AutoCategory`] 六个类别之一：
//! `URL` / `Email` / `path` / `code` / `JSON` / `plain`。
//!
//! 分类顺序是**有意**固定的（见 [`classify`] 内注释）：
//! - URL 在 Email 之前 —— `mailto:foo@bar.com` 应归为 URL 而非 Email；
//! - path 在 JSON 之前 —— 带引号的路径有可能被 `serde_json` 解析成功。

use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

use regex::Regex;

/// 自动识别出的内容类别。
///
/// [`AutoCategory::as_str`] 返回写入 SQLite `auto_category` 列的稳定字符串标签，
/// 大小写与 R4 / AE1 约定保持一致（`URL` / `Email` / `path` / `code` / `JSON` / `plain`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoCategory {
    /// 可被 `url::Url::parse` 解析且 scheme 长度 ≥ 2 的 URL（含 `mailto:` 等）。
    Url,
    /// 形如 `local@domain.tld` 的单一邮件地址。
    Email,
    /// Unix 路径前缀（`/`、`~/`)或 Windows 盘符（`C:\` / `C:/`）。
    Path,
    /// 代码特征符比例超过阈值的片段。
    Code,
    /// 可被 `serde_json` 解析为合法 JSON 值的文本。
    Json,
    /// 兜底类别。
    Plain,
}

impl AutoCategory {
    /// 返回写入 SQLite `auto_category` 列的稳定字符串标签。
    pub fn as_str(self) -> &'static str {
        match self {
            AutoCategory::Url => "URL",
            AutoCategory::Email => "Email",
            AutoCategory::Path => "path",
            AutoCategory::Code => "code",
            AutoCategory::Json => "JSON",
            AutoCategory::Plain => "plain",
        }
    }
}

/// 代码特征符占比阈值。
///
/// 取 **0.18**：AE1 的代码样本 `def foo(x): return x + 1` 去空白后 19 个字符，
/// 其中 `(` `)` `:` `+` 共 4 个特征符，比例 ≈ 0.21 > 0.18 → 归为 `code`；
/// 而普通英文散文（如 `hello world`）特征符比例接近 0 → 归为 `plain`。
/// 该阈值在“把代码识别出来”与“不误伤自然语言”之间取平衡，偏保守。
const CODE_SCORE_THRESHOLD: f64 = 0.18;

/// 对一段文本执行自动分类。
///
/// 分类顺序**不可随意调整**，理由见模块级文档。
pub fn classify(text: &str) -> AutoCategory {
    let s = text.trim();
    if s.is_empty() {
        return AutoCategory::Plain;
    }
    // URL 优先于 Email：mailto: 是一个合法 URL scheme。
    if is_url(s) {
        return AutoCategory::Url;
    }
    if looks_like_email(s) {
        return AutoCategory::Email;
    }
    // path 优先于 JSON：带引号的路径字符串可能被 serde_json 当作合法 JSON string 解析。
    if s.starts_with('/') || s.starts_with("~/") || has_windows_drive(s) {
        return AutoCategory::Path;
    }
    if serde_json::from_str::<serde_json::Value>(s).is_ok() {
        return AutoCategory::Json;
    }
    if compute_code_score(s) > CODE_SCORE_THRESHOLD {
        return AutoCategory::Code;
    }
    AutoCategory::Plain
}

/// 是否为 URL。
///
/// 在 `url::Url::parse` 成功之外，额外要求 scheme 长度 ≥ 2：
/// `url` crate 会把 Windows 盘符 `C:\...` 解析成 scheme 为 `c` 的 URL，
/// 单字母 scheme 过滤掉这种误判，同时保留 `http`/`https`/`mailto`/`file` 等真实 scheme。
fn is_url(s: &str) -> bool {
    match url::Url::parse(s) {
        Ok(u) => u.scheme().len() >= 2,
        Err(_) => false,
    }
}

/// 简单邮件启发：单个 token，`local@domain.tld` 形态，无空白。
fn looks_like_email(s: &str) -> bool {
    static EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[^@\s]+@[^@\s]+\.[^@\s]+$").expect("email regex must compile")
    });
    EMAIL_RE.is_match(s)
}

/// 是否以 Windows 盘符开头（`C:\` 或 `C:/`）。
fn has_windows_drive(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

/// 计算代码特征符占非空白字符的比例。
///
/// 特征符集合覆盖常见运算符 / 分隔符 / 块符号，但**刻意排除** `,` `.` `-` `_`
/// 以及引号，以免自然语言（逗号、句点、连字符、缩写)被误判为代码。
fn compute_code_score(s: &str) -> f64 {
    let total = s.chars().filter(|c| !c.is_whitespace()).count();
    if total == 0 {
        return 0.0;
    }
    let code_chars = s
        .chars()
        .filter(|c| {
            matches!(
                c,
                '{' | '}'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | ';'
                    | ':'
                    | '='
                    | '<'
                    | '>'
                    | '+'
                    | '*'
                    | '/'
                    | '\\'
                    | '&'
                    | '|'
                    | '%'
                    | '!'
                    | '#'
                    | '$'
                    | '@'
                    | '^'
                    | '~'
                    | '`'
            )
        })
        .count();
    code_chars as f64 / total as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ae1_samples() {
        // AE1 的四段输入 → URL / code / JSON / plain。
        assert_eq!(
            classify("https://example.com/api/v1/items?limit=20"),
            AutoCategory::Url
        );
        assert_eq!(classify("def foo(x): return x + 1"), AutoCategory::Code);
        assert_eq!(classify(r#"{"a": 1}"#), AutoCategory::Json);
        assert_eq!(classify("hello world"), AutoCategory::Plain);
    }

    #[test]
    fn url_variants() {
        assert_eq!(classify("http://foo.bar"), AutoCategory::Url);
        assert_eq!(classify("https://foo.bar/baz?q=1#frag"), AutoCategory::Url);
        assert_eq!(classify("ftp://host/file"), AutoCategory::Url);
    }

    #[test]
    fn mailto_is_url_not_email() {
        // URL 优先于 Email。
        assert_eq!(classify("mailto:foo@example.com"), AutoCategory::Url);
    }

    #[test]
    fn email_detection() {
        assert_eq!(classify("foo@example.com"), AutoCategory::Email);
        assert_eq!(classify("a.b+tag@sub.example.co"), AutoCategory::Email);
        // 含空白或缺少域名点的不算邮件。
        assert_ne!(classify("foo @ example.com"), AutoCategory::Email);
        assert_ne!(classify("foo@localhost"), AutoCategory::Email);
    }

    #[test]
    fn path_detection() {
        assert_eq!(classify("/usr/local/bin"), AutoCategory::Path);
        assert_eq!(classify("~/Documents/file.txt"), AutoCategory::Path);
        assert_eq!(classify(r"C:\Users\leo\file.txt"), AutoCategory::Path);
        assert_eq!(classify("D:/data/db"), AutoCategory::Path);
    }

    #[test]
    fn json_detection() {
        assert_eq!(classify(r#"{"a": 1, "b": [1,2,3]}"#), AutoCategory::Json);
        assert_eq!(classify("[1, 2, 3]"), AutoCategory::Json);
    }

    #[test]
    fn code_detection() {
        assert_eq!(classify("if (x > 0) { return x * 2; }"), AutoCategory::Code);
        assert_eq!(
            classify("let sum: i32 = a + b; println!(\"{}\", sum);"),
            AutoCategory::Code
        );
    }

    #[test]
    fn plain_fallback() {
        assert_eq!(classify("hello world"), AutoCategory::Plain);
        assert_eq!(
            classify("just some ordinary sentence here"),
            AutoCategory::Plain
        );
        assert_eq!(classify(""), AutoCategory::Plain);
        assert_eq!(classify("   "), AutoCategory::Plain);
    }

    #[test]
    fn as_str_labels() {
        assert_eq!(AutoCategory::Url.as_str(), "URL");
        assert_eq!(AutoCategory::Email.as_str(), "Email");
        assert_eq!(AutoCategory::Path.as_str(), "path");
        assert_eq!(AutoCategory::Code.as_str(), "code");
        assert_eq!(AutoCategory::Json.as_str(), "JSON");
        assert_eq!(AutoCategory::Plain.as_str(), "plain");
    }
}
