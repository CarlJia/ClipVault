//! 内容类型枚举（V3 schema `content_kind` 列的稳定字符串标签）。
//!
//! 与 `Entry::auto_category` 正交：`auto_category` 描述「这段文本是什么」
//! （URL / Email / path / code / JSON / plain），`content_kind` 描述「
//! NSPasteboard 给的是哪种二进制表示」（text / image / file / rtf / color）。
//! 文本条目的 `content_kind` 始终是 `Text`，其 `auto_category` 仍是分类
//! 结果；非文本条目的 `auto_category` 与 `content_kind` 字符串相同
//! （如 `image` 条目 `auto_category = "image"`）。

use serde::{Deserialize, Serialize};

/// 条目的内容类型。
///
/// [`ContentKind::as_str`] 返回写入 SQLite `content_kind` 列的稳定字符串，
/// 与 V3 schema 的迁移语义保持一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentKind {
    /// 纯文本（URL/Email/path/code/JSON/plain 五种 auto_category 共享）。
    Text,
    /// 图片（PNG/JPEG/TIFF 等）。
    Image,
    /// 文件路径数组（来自 NSFilenamesPboardType）。
    File,
    /// 富文本片段（来自 NSPasteboardTypeRTF）。
    Rtf,
    /// 拾色器值（来自 NSPasteboardTypeColor）。
    Color,
}

impl ContentKind {
    /// 写入 SQLite `content_kind` 列的稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            ContentKind::Text => "text",
            ContentKind::Image => "image",
            ContentKind::File => "file",
            ContentKind::Rtf => "rtf",
            ContentKind::Color => "color",
        }
    }

    /// 反向解析 SQLite `content_kind` 列的字符串。未知字符串按 `Text` 处理。
    pub fn parse(s: &str) -> Self {
        match s {
            "image" => ContentKind::Image,
            "file" => ContentKind::File,
            "rtf" => ContentKind::Rtf,
            "color" => ContentKind::Color,
            _ => ContentKind::Text,
        }
    }
}

impl Default for ContentKind {
    /// 默认值：`Text`。当 schema 升级时 V2 行的 `content_kind` 通过
    /// `DEFAULT 'text'` 自动填充。
    fn default() -> Self {
        ContentKind::Text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_labels() {
        assert_eq!(ContentKind::Text.as_str(), "text");
        assert_eq!(ContentKind::Image.as_str(), "image");
        assert_eq!(ContentKind::File.as_str(), "file");
        assert_eq!(ContentKind::Rtf.as_str(), "rtf");
        assert_eq!(ContentKind::Color.as_str(), "color");
    }

    #[test]
    fn parse_round_trip() {
        for kind in [
            ContentKind::Text,
            ContentKind::Image,
            ContentKind::File,
            ContentKind::Rtf,
            ContentKind::Color,
        ] {
            assert_eq!(ContentKind::parse(kind.as_str()), kind);
        }
    }

    #[test]
    fn parse_unknown_defaults_to_text() {
        assert_eq!(ContentKind::parse(""), ContentKind::Text);
        assert_eq!(ContentKind::parse("garbage"), ContentKind::Text);
    }

    #[test]
    fn default_is_text() {
        assert_eq!(ContentKind::default(), ContentKind::Text);
    }
}
