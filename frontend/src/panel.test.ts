//! W5 / R24：`escapeHtml` 改成单遍 `replace` + 映射表后，行为必须与原 5 段链式
//! 实现**逐字等价**。这条不变量是这次改动唯一的正确性约束 —— 转义漏一个字符就是
//! XSS 面，多转义一个字符就是显示错乱；两者都在下面的样本里钉住。
//!
//! 没有 DOM 依赖：`escapeHtml` 是纯函数。

import { describe, expect, it } from "vitest";

import { type EntryDto, escapeHtml, renderEntries, renderEntryRow } from "./panel";

/** 原实现（5 段链式 `replace`）——等价性基准。 */
function escapeHtmlReference(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

describe("escapeHtml（W5 / R24 单遍实现）", () => {
  const samples = [
    "",
    "plain text",
    "a & b",
    "<script>alert(1)</script>",
    `"double" and 'single'`,
    "&<>\"'", // 全部 5 个元字符
    "&&&<<<>>>\"\"\"'''", // 重复
    "a&amp;b", // 已经是实体：仍要再转义（与旧行为一致，避免双重未转义）
    "日本語 & emoji 🎉",
    "  leading and trailing  ",
    "/path/to/file?q=1&r=2",
  ];

  it("与原链式实现逐字等价", () => {
    for (const s of samples) {
      expect(escapeHtml(s), `输入 ${JSON.stringify(s)}`).toBe(
        escapeHtmlReference(s),
      );
    }
  });

  it("逐个钉住 5 个 HTML 元字符的实体形式", () => {
    expect(escapeHtml("&")).toBe("&amp;");
    expect(escapeHtml("<")).toBe("&lt;");
    expect(escapeHtml(">")).toBe("&gt;");
    expect(escapeHtml('"')).toBe("&quot;");
    expect(escapeHtml("'")).toBe("&#39;");
  });

  it("base64 data URL 原样通过（不引入新转义）", () => {
    // 后端缩略图 / 预览图走的正是这种串；W5 / R22 之后这类串不再过 escapeHtml，
    // 但即便过了也必须逐字不变 —— 这条钉住「字母表天然安全」。
    const b64 =
      "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
    expect(escapeHtml(b64)).toBe(b64);
    // 纯载荷（不含 data URL 前缀）同样不变。
    const payload = b64.replace("data:image/png;base64,", "");
    expect(escapeHtml(payload)).toBe(payload);
  });
});

// ---------------------------------------------------------------------------
// 单行渲染（W6）
// ---------------------------------------------------------------------------
//
// `renderEntryRow` 是从 `renderEntries` 里抽出来的：编辑备注保存后只替换那一行的
// `outerHTML`，不再 50 行全量重画。两条实现共用同一份行渲染逻辑是刻意的 ——
// 下面第一条测试钉住「两处不会漂移」。

/** `renderEntries` 只读写 `innerHTML`，最小替身够用。 */
function fakeContainer(): HTMLElement {
  return { innerHTML: "" } as unknown as HTMLElement;
}

function entry(over: Partial<EntryDto> & { id: number }): EntryDto {
  return {
    content: "",
    note: "",
    auto_category: "plain",
    tags: [],
    is_pinned: false,
    pinned_at: null,
    is_vault: false,
    is_decryptable: true,
    created_at: 0,
    content_kind: "text",
    mime_type: null,
    thumb: null,
    metadata: null,
    ...over,
  };
}

describe("renderEntryRow（W6 单行渲染）", () => {
  it("renderEntries 的输出 == 逐行 renderEntryRow 的拼接（两处不漂移）", () => {
    const entries = [
      entry({ id: 1, content: "第一条" }),
      entry({ id: 2, content: "第二条", content_kind: "image", thumb: "AAAA" }),
      entry({ id: 3, content: "第三条", note: "有备注", is_pinned: true }),
    ];
    const container = fakeContainer();
    renderEntries(container, entries, 1);
    expect(container.innerHTML).toBe(
      entries.map((e, i) => renderEntryRow(e, i, 1)).join(""),
    );
  });

  it("data-id / data-index 与 selected 标记按参数写入", () => {
    const html = renderEntryRow(entry({ id: 7 }), 3, 3);
    expect(html).toContain('data-id="7"');
    expect(html).toContain('data-index="3"');
    expect(html).toContain("selected");
    // 同一条但未选中时：不该带 selected。
    const unselected = renderEntryRow(entry({ id: 7 }), 3, 0);
    expect(unselected).not.toContain("selected");
  });

  it("空列表走空态分支（不产出任何行）", () => {
    const container = fakeContainer();
    renderEntries(container, [], 0);
    expect(container.innerHTML).toContain("没有匹配的记录");
    expect(container.innerHTML).not.toContain('class="row ');
  });
});
