// app/src/capture_test.rs
//
// capture.rs 因内联测试模块越过 1000 行，测试整体搬到这里，与既有
// pasteback_test.rs / winclip_test.rs 的约定一致。sibling 测试模块看不到另一模块的
// 私有项，所以被测试驱动的 helper 在 capture.rs 里是 `pub(crate)`（不扩大 crate 外 API）。
//
// 内容相对上游：以 capture.rs 内联模块为准（含 RTF 纯文本兜底的用例），
// 外加 urls_from_objects 的过滤分支用例。

#[cfg(test)]
mod tests {
    use crate::capture::*;
    use clipvault_core::ContentKind;

    #[test]
    fn text_captured_round_trip() {
        let c = Captured::text("hello");
        assert_eq!(c.kind, ContentKind::Text);
        assert_eq!(c.text.as_deref(), Some("hello"));
        assert!(c.payload.is_none());
        assert!(c.fingerprint().len() == 16);
    }

    #[test]
    fn file_captured_serializes_paths_into_payload() {
        let c = Captured::file(vec!["/a.pdf".into(), "/b.csv".into()], true);
        let payload = c.payload.as_ref().unwrap();
        let back: Vec<String> = serde_json::from_slice(payload).unwrap();
        assert_eq!(back, vec!["/a.pdf", "/b.csv"]);
        assert_eq!(c.metadata["paths"][0], "/a.pdf");
        assert_eq!(c.metadata["resolved"], true);
    }

    #[test]
    fn color_captured_carries_rgba_only() {
        let c = Captured::color("rgba(255,0,0,1)");
        assert_eq!(c.kind, ContentKind::Color);
        assert_eq!(c.metadata["rgba"], "rgba(255,0,0,1)");
        assert!(c.payload.is_none());
    }

    /// 真实粘贴板颜色归档(sRGB 0.2/0.6/0.9/1.0 的 `NSKeyedArchiver` bplist)。
    ///
    /// 生成方式与生产同形:把该 `NSColor` 写进一个私有 `NSPasteboard`,再读
    /// `NSPasteboardTypeColor` 的字节。夹具写死而不是测试里现造,是为了让用例只测我们的
    /// 解码路径,不依赖 AppKit 的写侧 API。
    #[cfg(target_os = "macos")]
    const NSCOLOR_SRGB_ARCHIVE_B64: &str = concat!(
        "YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8Q",
        "D05TS2V5ZWRBcmNoaXZlctEICVRyb290gAGmCwwZHyAnVSRudWxs1g0ODxAREhMUFRYXGFYkY2xh",
        "c3NcTlNDb21wb25lbnRzVU5TUkdCXE5TQ29sb3JTcGFjZV8QEk5TQ3VzdG9tQ29sb3JTcGFjZV8Q",
        "EE5TTGluZWFyRXhwb3N1cmWABU0wLjIgMC42IDAuOSAxTxAnMC4xNjU1NTAzMzYyIDAuNTIwMDI5",
        "MTg3MiAwLjg3NDQyMjA3MzQAEAGAAkEw0xobDRwdHlROU0lEVU5TSUNDEAeAA4AETxEMSAAADEhM",
        "aW5vAhAAAG1udHJSR0IgWFlaIAfOAAIACQAGADEAAGFjc3BNU0ZUAAAAAElFQyBzUkdCAAAAAAAA",
        "AAAAAAAAAAD21gABAAAAANMtSFAgIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "AAAAAAAAAAAAAAAAEWNwcnQAAAFQAAAAM2Rlc2MAAAGEAAAAbHd0cHQAAAHwAAAAFGJrcHQAAAIE",
        "AAAAFHJYWVoAAAIYAAAAFGdYWVoAAAIsAAAAFGJYWVoAAAJAAAAAFGRtbmQAAAJUAAAAcGRtZGQA",
        "AALEAAAAiHZ1ZWQAAANMAAAAhnZpZXcAAAPUAAAAJGx1bWkAAAP4AAAAFG1lYXMAAAQMAAAAJHRl",
        "Y2gAAAQwAAAADHJUUkMAAAQ8AAAIDGdUUkMAAAQ8AAAIDGJUUkMAAAQ8AAAIDHRleHQAAAAAQ29w",
        "eXJpZ2h0IChjKSAxOTk4IEhld2xldHQtUGFja2FyZCBDb21wYW55AABkZXNjAAAAAAAAABJzUkdC",
        "IElFQzYxOTY2LTIuMQAAAAAAAAAAAAAAEnNSR0IgSUVDNjE5NjYtMi4xAAAAAAAAAAAAAAAAAAAA",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABYWVogAAAAAAAA81EAAQAAAAEWzFhZ",
        "WiAAAAAAAAAAAAAAAAAAAAAAWFlaIAAAAAAAAG+iAAA49QAAA5BYWVogAAAAAAAAYpkAALeFAAAY",
        "2lhZWiAAAAAAAAAkoAAAD4QAALbPZGVzYwAAAAAAAAAWSUVDIGh0dHA6Ly93d3cuaWVjLmNoAAAA",
        "AAAAAAAAAAAWSUVDIGh0dHA6Ly93d3cuaWVjLmNoAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "AAAAAAAAAAAAAAAAAAAAAAAAAGRlc2MAAAAAAAAALklFQyA2MTk2Ni0yLjEgRGVmYXVsdCBSR0Ig",
        "Y29sb3VyIHNwYWNlIC0gc1JHQgAAAAAAAAAAAAAALklFQyA2MTk2Ni0yLjEgRGVmYXVsdCBSR0Ig",
        "Y29sb3VyIHNwYWNlIC0gc1JHQgAAAAAAAAAAAAAAAAAAAAAAAAAAAABkZXNjAAAAAAAAACxSZWZl",
        "cmVuY2UgVmlld2luZyBDb25kaXRpb24gaW4gSUVDNjE5NjYtMi4xAAAAAAAAAAAAAAAsUmVmZXJl",
        "bmNlIFZpZXdpbmcgQ29uZGl0aW9uIGluIElFQzYxOTY2LTIuMQAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "AAAAAAAAdmlldwAAAAAAE6T+ABRfLgAQzxQAA+3MAAQTCwADXJ4AAAABWFlaIAAAAAAATAlWAFAA",
        "AABXH+dtZWFzAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAAAAAACjwAAAAJzaWcgAAAAAENSVCBjdXJ2",
        "AAAAAAAABAAAAAAFAAoADwAUABkAHgAjACgALQAyADcAOwBAAEUASgBPAFQAWQBeAGMAaABtAHIA",
        "dwB8AIEAhgCLAJAAlQCaAJ8ApACpAK4AsgC3ALwAwQDGAMsA0ADVANsA4ADlAOsA8AD2APsBAQEH",
        "AQ0BEwEZAR8BJQErATIBOAE+AUUBTAFSAVkBYAFnAW4BdQF8AYMBiwGSAZoBoQGpAbEBuQHBAckB",
        "0QHZAeEB6QHyAfoCAwIMAhQCHQImAi8COAJBAksCVAJdAmcCcQJ6AoQCjgKYAqICrAK2AsECywLV",
        "AuAC6wL1AwADCwMWAyEDLQM4A0MDTwNaA2YDcgN+A4oDlgOiA64DugPHA9MD4APsA/kEBgQTBCAE",
        "LQQ7BEgEVQRjBHEEfgSMBJoEqAS2BMQE0wThBPAE/gUNBRwFKwU6BUkFWAVnBXcFhgWWBaYFtQXF",
        "BdUF5QX2BgYGFgYnBjcGSAZZBmoGewaMBp0GrwbABtEG4wb1BwcHGQcrBz0HTwdhB3QHhgeZB6wH",
        "vwfSB+UH+AgLCB8IMghGCFoIbgiCCJYIqgi+CNII5wj7CRAJJQk6CU8JZAl5CY8JpAm6Cc8J5Qn7",
        "ChEKJwo9ClQKagqBCpgKrgrFCtwK8wsLCyILOQtRC2kLgAuYC7ALyAvhC/kMEgwqDEMMXAx1DI4M",
        "pwzADNkM8w0NDSYNQA1aDXQNjg2pDcMN3g34DhMOLg5JDmQOfw6bDrYO0g7uDwkPJQ9BD14Peg+W",
        "D7MPzw/sEAkQJhBDEGEQfhCbELkQ1xD1ERMRMRFPEW0RjBGqEckR6BIHEiYSRRJkEoQSoxLDEuMT",
        "AxMjE0MTYxODE6QTxRPlFAYUJxRJFGoUixStFM4U8BUSFTQVVhV4FZsVvRXgFgMWJhZJFmwWjxay",
        "FtYW+hcdF0EXZReJF64X0hf3GBsYQBhlGIoYrxjVGPoZIBlFGWsZkRm3Gd0aBBoqGlEadxqeGsUa",
        "7BsUGzsbYxuKG7Ib2hwCHCocUhx7HKMczBz1HR4dRx1wHZkdwx3sHhYeQB5qHpQevh7pHxMfPh9p",
        "H5Qfvx/qIBUgQSBsIJggxCDwIRwhSCF1IaEhziH7IiciVSKCIq8i3SMKIzgjZiOUI8Ij8CQfJE0k",
        "fCSrJNolCSU4JWgllyXHJfcmJyZXJocmtyboJxgnSSd6J6sn3CgNKD8ocSiiKNQpBik4KWspnSnQ",
        "KgIqNSpoKpsqzysCKzYraSudK9EsBSw5LG4soizXLQwtQS12Last4S4WLkwugi63Lu4vJC9aL5Ev",
        "xy/+MDUwbDCkMNsxEjFKMYIxujHyMioyYzKbMtQzDTNGM38zuDPxNCs0ZTSeNNg1EzVNNYc1wjX9",
        "Njc2cjauNuk3JDdgN5w31zgUOFA4jDjIOQU5Qjl/Obw5+To2OnQ6sjrvOy07azuqO+g8JzxlPKQ8",
        "4z0iPWE9oT3gPiA+YD6gPuA/IT9hP6I/4kAjQGRApkDnQSlBakGsQe5CMEJyQrVC90M6Q31DwEQD",
        "REdEikTORRJFVUWaRd5GIkZnRqtG8Ec1R3tHwEgFSEtIkUjXSR1JY0mpSfBKN0p9SsRLDEtTS5pL",
        "4kwqTHJMuk0CTUpNk03cTiVObk63TwBPSU+TT91QJ1BxULtRBlFQUZtR5lIxUnxSx1MTU19TqlP2",
        "VEJUj1TbVShVdVXCVg9WXFapVvdXRFeSV+BYL1h9WMtZGllpWbhaB1pWWqZa9VtFW5Vb5Vw1XIZc",
        "1l0nXXhdyV4aXmxevV8PX2Ffs2AFYFdgqmD8YU9homH1YklinGLwY0Njl2PrZEBklGTpZT1lkmXn",
        "Zj1mkmboZz1nk2fpaD9olmjsaUNpmmnxakhqn2r3a09rp2v/bFdsr20IbWBtuW4SbmtuxG8eb3hv",
        "0XArcIZw4HE6cZVx8HJLcqZzAXNdc7h0FHRwdMx1KHWFdeF2Pnabdvh3VnezeBF4bnjMeSp5iXnn",
        "ekZ6pXsEe2N7wnwhfIF84X1BfaF+AX5ifsJ/I3+Ef+WAR4CogQqBa4HNgjCCkoL0g1eDuoQdhICE",
        "44VHhauGDoZyhteHO4efiASIaYjOiTOJmYn+imSKyoswi5aL/IxjjMqNMY2Yjf+OZo7OjzaPnpAG",
        "kG6Q1pE/kaiSEZJ6kuOTTZO2lCCUipT0lV+VyZY0lp+XCpd1l+CYTJi4mSSZkJn8mmia1ZtCm6+c",
        "HJyJnPedZJ3SnkCerp8dn4uf+qBpoNihR6G2oiailqMGo3aj5qRWpMelOKWpphqmi6b9p26n4KhS",
        "qMSpN6mpqhyqj6sCq3Wr6axcrNCtRK24ri2uoa8Wr4uwALB1sOqxYLHWskuywrM4s660JbSctRO1",
        "irYBtnm28Ldot+C4WbjRuUq5wro7urW7LrunvCG8m70VvY++Cr6Evv+/er/1wHDA7MFnwePCX8Lb",
        "w1jD1MRRxM7FS8XIxkbGw8dBx7/IPci8yTrJuco4yrfLNsu2zDXMtc01zbXONs62zzfPuNA50LrR",
        "PNG+0j/SwdNE08bUSdTL1U7V0dZV1tjXXNfg2GTY6Nls2fHadtr724DcBdyK3RDdlt4c3qLfKd+v",
        "4DbgveFE4cziU+Lb42Pj6+Rz5PzlhOYN5pbnH+ep6DLovOlG6dDqW+rl63Dr++yG7RHtnO4o7rTv",
        "QO/M8Fjw5fFy8f/yjPMZ86f0NPTC9VD13vZt9vv3ivgZ+Kj5OPnH+lf65/t3/Af8mP0p/br+S/7c",
        "/23//9IhIiMkWiRjbGFzc25hbWVYJGNsYXNzZXNcTlNDb2xvclNwYWNloiUmXE5TQ29sb3JTcGFj",
        "ZVhOU09iamVjdNIhIigpV05TQ29sb3KiKCYACAARABoAJAApADIANwBJAEwAUQBTAFoAYABtAHQA",
        "gQCHAJQAqQC8AL4AzAD2APgA+gD8AQMBCAEOARABEgEUDWANZQ1wDXkNhg2JDZYNnw2kDawAAAAA",
        "AAACAQAAAAAAAAAqAAAAAAAAAAAAAAAAAAANrw==",
    );

    #[cfg(target_os = "macos")]
    fn nscolor_fixture() -> Vec<u8> {
        use base64::Engine;

        base64::engine::general_purpose::STANDARD
            .decode(NSCOLOR_SRGB_ARCHIVE_B64)
            .expect("夹具必须是合法 base64")
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn color_capture_decodes_to_srgb_rgba() {
        // 回归:修前 color 的身份是归档字节长度占位(`nskeyed:{}B`),而实测红与绿的
        // NSColor 归档都是 3603 字节 —— 等长即同指纹,第二个颜色会被 content_hash
        // UNIQUE 静默丢弃,和图片侧是同一个缺陷。
        let captured = color_capture(&nscolor_fixture());
        assert_eq!(captured.kind, ContentKind::Color);
        assert_eq!(
            captured.metadata["rgba"], "rgba(51,153,230,1)",
            "身份必须来自解码出的颜色内容,而不是归档有多长"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn color_capture_falls_back_to_a_content_digest() {
        // 解不开归档的颜色也不能退回长度占位:两个**等长**但内容不同的垃圾输入必须
        // 得到不同指纹(长度占位会让它们相同,静默丢弃就换个形式回来了)。
        let a = color_capture(b"not a color archive!!");
        let b = color_capture(b"not a color archive??");
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert!(
            a.metadata["rgba"]
                .as_str()
                .is_some_and(|s| s.starts_with("nskeyed:")),
            "兜底仍是归档摘要占位串"
        );
    }

    #[test]
    fn image_captured_records_dimensions_and_size() {
        // 1x1 transparent PNG
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89";
        let c = Captured::image("image/png", png.to_vec(), 1, 1);
        assert_eq!(c.kind, ContentKind::Image);
        assert_eq!(c.metadata["width"], 1);
        assert_eq!(c.metadata["height"], 1);
        assert_eq!(c.metadata["size_bytes"], png.len() as u64);
    }

    #[test]
    fn fingerprint_differs_across_kinds_for_same_payload() {
        let bytes = b"identical-bytes".to_vec();
        let text = Captured::text("identical-bytes");
        let file = Captured::file(vec!["identical-bytes".into()], false);
        let image = Captured::image("image/png", bytes.clone(), 1, 1);
        let rtf = Captured::rtf(bytes, None);
        assert_ne!(text.fingerprint(), file.fingerprint());
        assert_ne!(file.fingerprint(), image.fingerprint());
        assert_ne!(image.fingerprint(), rtf.fingerprint());
        assert_ne!(rtf.fingerprint(), text.fingerprint());
    }

    #[test]
    fn fingerprint_differs_within_kind_for_different_payloads() {
        // 回归：binary 分支曾只把 kind 名喂进摘要,同 kind 的第二次捕获会得到
        // 完全相同的指纹,撞 entries.content_hash UNIQUE 后被静默丢弃 ——
        // 用户可见症状就是「复制图片不进历史」(只有第一张进得去)。
        let img_a = Captured::image("image/png", b"png-bytes-a".to_vec(), 1, 1);
        let img_b = Captured::image("image/png", b"png-bytes-b".to_vec(), 1, 1);
        assert_ne!(
            img_a.fingerprint(),
            img_b.fingerprint(),
            "同 kind 不同 payload 必须得到不同指纹"
        );

        let file_a = Captured::file(vec!["/tmp/a.txt".into()], true);
        let file_b = Captured::file(vec!["/tmp/b.txt".into()], true);
        assert_ne!(file_a.fingerprint(), file_b.fingerprint());

        let rtf_a = Captured::rtf(b"{\\rtf1 a}".to_vec(), None);
        let rtf_b = Captured::rtf(b"{\\rtf1 b}".to_vec(), None);
        assert_ne!(rtf_a.fingerprint(), rtf_b.fingerprint());

        // 指纹必须仍然是 payload 的确定函数:5 秒去重窗口靠它判「同一条内容」,
        // 换个不稳定来源(时间戳、地址)会让去重整体失效。
        assert_eq!(
            img_a.fingerprint(),
            Captured::image("image/png", b"png-bytes-a".to_vec(), 1, 1).fingerprint(),
            "同 kind 同 payload 的指纹必须稳定"
        );
    }

    #[test]
    fn png_dimensions_parses_1x1() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89";
        assert_eq!(png_dimensions(png), Some((1, 1)));
    }

    #[test]
    fn png_dimensions_rejects_jpeg_signature() {
        let jpeg = b"\xff\xd8\xff\xe0\x00\x10JFIF";
        assert!(png_dimensions(jpeg).is_none());
    }

    #[test]
    fn tiff_dimensions_parses_le_2x3() {
        // II* little-endian, width=2, height=3
        let mut tiff = vec![b'I', b'I', 0x2a, 0x00];
        tiff.extend(2u32.to_le_bytes());
        tiff.extend(3u32.to_le_bytes());
        assert_eq!(tiff_dimensions(&tiff), Some((2, 3)));
    }

    #[test]
    fn tiff_dimensions_parses_be_4x5() {
        let mut tiff = vec![b'M', b'M', 0x00, 0x2a];
        tiff.extend(4u32.to_be_bytes());
        tiff.extend(5u32.to_be_bytes());
        assert_eq!(tiff_dimensions(&tiff), Some((4, 5)));
    }

    /// 构造一个文件 URL，作为 [`paths_from_urls`] 的测试输入
    /// （生产侧这个数组由 `readObjectsForClasses(_:options:)` 产出）。
    #[cfg(target_os = "macos")]
    fn url(path: &str) -> objc2::rc::Retained<objc2_foundation::NSURL> {
        use objc2_foundation::{NSString, NSURL};
        NSURL::fileURLWithPath(&NSString::from_str(path))
    }

    /// 解出 file payload 里的路径数组（与 `Captured::file` 的编码同源）。
    #[cfg(target_os = "macos")]
    fn payload_paths(captured: &Captured) -> Vec<String> {
        serde_json::from_slice(captured.payload.as_deref().unwrap_or(&[])).unwrap()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn paths_from_urls_resolves_existing_paths() {
        let dir = std::env::temp_dir();
        let dir_str = dir.to_str().expect("临时目录必须是 UTF-8");
        let expected = std::fs::canonicalize(&dir)
            .expect("临时目录必须存在")
            .to_str()
            .expect("规范路径必须是 UTF-8")
            .to_string();

        let captured = paths_from_urls(&[url(dir_str)]).expect("存在的目录应产出 Captured");

        assert_eq!(captured.kind, ContentKind::File);
        assert_eq!(payload_paths(&captured), vec![expected]);
        assert_eq!(captured.metadata["resolved"], true);
        assert_eq!(captured.mime.as_deref(), Some("text/uri-list"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn paths_from_urls_unresolvable_keeps_original_with_resolved_false() {
        let missing = "/nonexistent/path/that/does/not/exist";

        let captured =
            paths_from_urls(&[url(missing)]).expect("canonicalize 失败仍应产出 Captured");

        assert_eq!(payload_paths(&captured), vec![missing.to_string()]);
        assert_eq!(captured.metadata["resolved"], false);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn paths_from_urls_empty_returns_none() {
        assert!(
            paths_from_urls(&[]).is_none(),
            "空数组必须返回 None，让 watcher 走下一优先级"
        );
    }

    /// 回归:浏览器"拷贝链接"写下的 `public.url`(层级 URL)也是 `NSURL`,且 `path()`
    /// 在它上面不是 nil(`https://example.com/page` → `/page`)。混在文件 URL 里时必须
    /// 被 `isFileURL` 过滤掉,不能当成假文件路径采集。
    #[cfg(target_os = "macos")]
    #[test]
    fn paths_from_urls_skips_non_file_urls() {
        use objc2_foundation::{NSString, NSURL};

        let link = NSURL::URLWithString(&NSString::from_str("https://example.com/page"))
            .expect("示例 URL 必须合法");

        let captured =
            paths_from_urls(&[url("/tmp"), link]).expect("至少一个文件 URL 应产出 Captured");

        let expected = std::fs::canonicalize("/tmp")
            .expect("/tmp 必须存在")
            .to_str()
            .expect("规范路径必须是 UTF-8")
            .to_string();
        assert_eq!(captured.kind, ContentKind::File);
        assert_eq!(
            payload_paths(&captured),
            vec![expected],
            "只应命中真正的文件路径,链接的 `/page` 不得混进来"
        );
    }

    /// 回归:粘贴板上只有非文件 URL 时必须返回 `None`,让链路落到 string 优先级去
    /// 捕获 URL 文本 —— 否则 URL 文本永久丢失、只剩一条假 File 条目。
    #[cfg(target_os = "macos")]
    #[test]
    fn paths_from_urls_all_non_file_returns_none() {
        use objc2_foundation::{NSString, NSURL};

        let link = NSURL::URLWithString(&NSString::from_str("https://example.com/page"))
            .expect("示例 URL 必须合法");

        assert!(
            paths_from_urls(&[link]).is_none(),
            "全是非文件 URL 时必须返回 None"
        );
    }

    /// 回归:只有文本(例如 URL 以 `public.utf8-plain-text` 落在粘贴板上)时,
    /// filenames 这一优先级不得命中,免得把普通文本采成 File 条目。
    ///
    /// 用**命名**粘贴板而不是 `generalPasteboard`(同
    /// [`read_objects_round_trips_filenames_written_as_property_list`] 的理由);名字与那个
    /// 测试不同,避免两个用例并发跑时互相 `clearContents` 造成 flake。
    #[cfg(target_os = "macos")]
    #[test]
    fn poll_filenames_returns_none_for_text_only_pasteboard() {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        use objc2_foundation::NSString;

        let pb = NSPasteboard::pasteboardWithName(&NSString::from_str("ClipVaultCaptureTextOnly"));
        pb.clearContents();

        let _ = unsafe {
            pb.setString_forType(
                &NSString::from_str("https://example.com/page"),
                NSPasteboardTypeString,
            )
        };

        assert!(
            poll_filenames(&pb).is_none(),
            "只有文本的粘贴板不得被采集为 File 条目"
        );
    }

    #[test]
    fn poll_first_non_empty_returns_none_when_change_count_unchanged() {
        // 非 macOS 路径返回 None;但 macOS 路径也会因 change_count 未增返回 None
        let result = poll_first_non_empty(5, 5);
        assert!(result.is_none());
    }

    /// U1 的核心假设的自动化证明：**一致地**写进 `.filenames` 的
    /// `NSArray<NSString>`（U2 的回贴写侧正是这个形状）能被
    /// `readObjectsForClasses(_:options:)` 以 `NSURL` 读回来、再由
    /// [`paths_from_urls`] 还原成路径数组。
    ///
    /// 这条链路之前在两边都是断的（采集侧扫 ASCII 字节、回贴侧只写纯文本），
    /// 单测各自"通过"却谁也没覆盖"写进去读得回来"。写侧直接调**生产**的
    /// [`crate::pasteback::write_file_paths_to_pasteboard`]，生产写侧一旦漂移这里就会红，
    /// 而不是对着测试里手搭的克隆写入器自说自话。
    ///
    /// 这里用**命名**粘贴板而不是 `generalPasteboard`：测试绝不碰用户正在用的剪贴板。
    /// 命名粘贴板会在本次登录会话里留存（objc2 0.3 未暴露 `releaseGlobally`），但它只装
    /// 两个路径、用户不可见。
    ///
    /// 它证明的是「我们自己的写入形状读得回来」；Finder 复制文件时粘贴板上还有哪些
    /// 附带类型，仍归真机 AE1 验收。
    #[cfg(target_os = "macos")]
    #[test]
    fn read_objects_round_trips_filenames_written_as_property_list() {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        use objc2_foundation::NSString;

        // 取两个一定存在的路径（macOS 上 /tmp 是指向 /private/tmp 的符号链接，
        // 正好顺带覆盖 canonicalize 这一步）。
        let sources = ["/tmp", "/usr"];

        let pb = NSPasteboard::pasteboardWithName(&NSString::from_str("ClipVaultCaptureTest"));
        pb.clearContents();

        // 走生产写侧（与 `MacosBackend::write_clipboard` 的 File 分支同一份实现）。
        let paths: Vec<String> = sources.iter().map(|p| p.to_string()).collect();
        crate::pasteback::write_file_paths_to_pasteboard(&pb, &paths);

        // 双写契约：文件引用之外还必须无条件挂纯文本表示（路径按 `\n` 拼接），
        // 给只认文本的接收方（此处此前无人证明）。
        let string_repr = pb.stringForType(unsafe { NSPasteboardTypeString });
        assert_eq!(
            string_repr.map(|s| s.to_string()),
            Some(paths.join("\n")),
            "写侧必须同时挂 `\\n` 拼接的纯文本表示"
        );

        let captured = poll_filenames(&pb).expect("刚写进去的文件引用应能被读回");
        assert_eq!(captured.kind, ContentKind::File);
        assert_eq!(captured.metadata["resolved"], true, "两个路径都存在");

        let mut got = payload_paths(&captured);
        got.sort();
        let mut expected: Vec<String> = sources
            .iter()
            .map(|p| {
                std::fs::canonicalize(p)
                    .expect("测试路径必须存在")
                    .to_str()
                    .expect("规范路径必须是 UTF-8")
                    .to_string()
            })
            .collect();
        expected.sort();
        assert_eq!(got, expected);
    }

    #[test]
    fn poll_first_non_empty_returns_none_when_change_count_decreased() {
        let result = poll_first_non_empty(3, 5);
        assert!(result.is_none());
    }

    /// 性能优化 W1 / R7:`insert_captured_with_metadata` 不再自己算
    /// `fingerprint()`,完全由调用方传入。watcher 在第 233 行已经算过
    /// 一次再传进来;这是一条端到端契约——`hash == "caller-provided"`
    /// 才说明我们没有偷跑一次 SHA-256。
    #[tokio::test]
    async fn insert_captured_with_metadata_uses_caller_hash_not_internal_fingerprint() {
        let storage = crate::storage::test_helpers::open_test_db();

        // 抓一个普通文本捕获。
        let captured = Captured::text("caller-supplied hash test");
        let caller_fingerprint = "callerprovided"; // 16 hex chars 风格, 长度任意
        let id = insert_captured_with_metadata(
            &storage,
            &captured,
            "plain",
            r#"{"note":"test"}"#,
            caller_fingerprint,
        )
        .await
        .unwrap();
        assert!(id > 0);

        // 直接读 entries.content_hash:必须等于调用方传入的值,**不应**
        // 等于 captured.fingerprint() 的实际结果(那是另一次 SHA-256)。
        let stored_hash: String = storage
            .with_conn(move |conn| {
                conn.query_row(
                    "SELECT content_hash FROM entries WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(
            stored_hash, caller_fingerprint,
            "rows.content_hash 必须严格等于调用方传入的 fingerprint, 等于就说明没偷跑 fingerprint()"
        );

        // 反向证明:captured.fingerprint() 的实际结果必须跟 stored_hash 不同——
        // 否则我们可能只是偷跑了一个等于传值的 fingerprint(), 仍算偷跑。
        let real_fp = captured.fingerprint();
        assert_ne!(
            stored_hash, real_fp,
            "rows.content_hash 绝不能等于 captured.fingerprint() 的真实结果（说明函数真的用 caller 值）"
        );
    }

    /// rtf 捕获必须带一份纯文本兜底，且它落在 `metadata_json` 而不是 `content`：
    /// `entries_fts` 的 INSERT 触发器对非 file 的二进制条目收的就是 `NEW.content`
    /// （`clipvault-core/src/schema.rs`），文本进了 `content` 就会被顺手变成可搜索
    /// 内容，与 R22「rtf 不入 FTS」冲突。
    #[test]
    fn rtf_capture_keeps_plain_text_in_metadata_not_content() {
        let captured = Captured::rtf(b"{\\rtf1 hi}".to_vec(), Some("hi".into()));

        assert_eq!(captured.kind, ContentKind::Rtf);
        assert!(
            captured.text.is_none(),
            "Captured.text 是条目的 content 位，必须留空"
        );
        assert_eq!(captured.metadata["text"], "hi");
    }

    /// 没有纯文本（只挂 RTF 的复制源）或空串时，metadata 里不留 `text` 键 ——
    /// 回贴侧据此只挂 RTF，行为与修复前一致。
    #[test]
    fn rtf_capture_omits_empty_text_fallback() {
        for text in [None, Some(String::new())] {
            let captured = Captured::rtf(b"{\\rtf1 hi}".to_vec(), text);
            assert!(
                captured.metadata.get("text").is_none(),
                "没有可用文本兜底时不该写一个空 text 键"
            );
        }
    }

    /// 端到端：rtf 捕获落库后 `content` 仍是空（不污染 FTS），纯文本在 metadata 里。
    #[tokio::test]
    async fn rtf_capture_round_trips_through_insert() {
        let storage = crate::storage::test_helpers::open_test_db();
        let captured = Captured::rtf(b"{\\rtf1 hi}".to_vec(), Some("hi".into()));
        let metadata_json = serde_json::to_string(&captured.metadata).unwrap();

        let id = insert_captured_with_metadata(
            &storage,
            &captured,
            "rtf",
            &metadata_json,
            &captured.fingerprint(),
        )
        .await
        .unwrap();

        let (content, stored_metadata): (String, Option<String>) = storage
            .with_conn(move |conn| {
                conn.query_row(
                    "SELECT content, metadata_json FROM entries WHERE id = ?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .await
            .unwrap();

        assert_eq!(content, "", "rtf 条目的 content 必须留空（R22：不入 FTS）");
        let meta: serde_json::Value =
            serde_json::from_str(stored_metadata.as_deref().unwrap_or("{}")).unwrap();
        assert_eq!(meta["text"], "hi", "纯文本兜底应随条目一起入库");
    }

    /// `readObjectsForClasses(_:options:)` 返回的数组静态类型是异质的 `AnyObject`
    /// 数组,非 `NSURL` 元素必须在 downcast 这一步被丢弃。
    ///
    /// 真实粘贴板造不出这个输入 —— 引擎层对 `[NSURL.class]` 只返回 `NSURL` 实例 ——
    /// 所以这里手工搭一个混入 `NSString` 的数组直接驱动过滤函数。若不测,
    /// 「混入别的类型会被当成 URL 用」这条就只能等线上撞见。
    #[cfg(target_os = "macos")]
    #[test]
    fn urls_from_objects_drops_non_url_elements() {
        use objc2::rc::Retained;
        use objc2::runtime::AnyObject;
        use objc2_foundation::{NSArray, NSString};

        // SAFETY: 向上转成类型擦除的 `AnyObject`（即 Obj-C 的 `id`）总是合法的，
        // 不改变对象的动态类型。这里是为了让两种不同的对象能放进同一个数组。
        let mixed: Vec<Retained<AnyObject>> =
            vec![unsafe { Retained::cast_unchecked(url("/tmp")) }, unsafe {
                Retained::cast_unchecked(NSString::from_str("just text"))
            }];

        let urls = urls_from_objects(&NSArray::from_retained_slice(&mixed));

        assert_eq!(urls.len(), 1, "非 NSURL 元素必须被丢弃");
        assert_eq!(
            urls[0].path().map(|p| p.to_string()).as_deref(),
            Some("/tmp")
        );

        // 全是非 NSURL 时得空数组,`paths_from_urls` 据此返回 None,
        // 链路继续走 image → rtf → color → string。
        let only_text: Vec<Retained<AnyObject>> =
            vec![unsafe { Retained::cast_unchecked(NSString::from_str("just text")) }];
        assert!(
            urls_from_objects(&NSArray::from_retained_slice(&only_text)).is_empty(),
            "没有任何 NSURL 时必须是空数组"
        );
    }
}
