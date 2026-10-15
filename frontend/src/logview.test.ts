// U4: 诊断日志区 pure helper 单测。KTD7「最多渲染 200 行」是这条函数的契约。

import { describe, expect, it } from "vitest";
import { MAX_VIEW_LINES, formatSize, formatTailForView } from "./logview";

describe("formatTailForView", () => {
  it("空串 → 空串", () => {
    expect(formatTailForView("")).toBe("");
  });

  it("行数不足 maxLines → 原文(加尾换行)", () => {
    const raw = "a\nb\nc\n";
    expect(formatTailForView(raw, 10)).toBe("a\nb\nc\n");
  });

  it("行数等于 maxLines → 原文(无省略提示)", () => {
    const raw = "a\nb\nc\n";
    expect(formatTailForView(raw, 3)).toBe("a\nb\nc\n");
  });

  it("行数超过 maxLines → 末段 + 省略提示,首部不出现", () => {
    const raw = Array.from({ length: 10 }, (_, i) => `line${i}`).join("\n") + "\n";
    const out = formatTailForView(raw, 3);
    expect(out.startsWith("…（已省略前 7 行）\n")).toBe(true);
    expect(out).toContain("line7");
    expect(out).toContain("line9");
    expect(out).not.toContain("line0");
    expect(out.endsWith("\n")).toBe(true);
  });

  it("使用默认 MAX_VIEW_LINES(200)", () => {
    expect(MAX_VIEW_LINES).toBe(200);
  });

  it("末尾无换行时仍按换行分割 + 末尾补 \\n", () => {
    const raw = "a\nb\nc"; // 无尾 \n
    const out = formatTailForView(raw, 10);
    // 3 行,无尾 \n,join 后加 \n
    expect(out).toBe("a\nb\nc\n");
  });
});

describe("formatSize", () => {
  it("B / KiB / MiB 边界", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(1023)).toBe("1023 B");
    expect(formatSize(1024)).toBe("1.0 KiB");
    expect(formatSize(1024 * 1024)).toBe("1.00 MiB");
    expect(formatSize(2 * 1024 * 1024 + 512 * 1024)).toBe("2.50 MiB");
  });
});
