// frontend/src/vault-search.test.ts
//
// 加密区搜索的纯函数护栏。
//
// 这些用例能存在，靠的是 `vault-search.ts` 里**没有 DOM 依赖** —— 前端测试跑在
// node 环境里（没有 jsdom），`main.ts` 顶部就执行 `document.querySelector`，根本
// 导入不了。过滤与选中收敛的语义因此必须住在一个纯函数模块里，测试才有东西可钉。
//
// 钉住的行为（对应 plan R2–R7）：
// - 词间「与」+ 大小写不敏感子串；
// - 只匹配备注 / 标签 / 类别 / 本地日期这四项（正文是密文，不解密就搜不到）；
// - 「查询变化」与「数据变化」两条路径的选中收敛规则不同。

import { describe, expect, it } from "vitest";
import {
  applyVaultQuery,
  filterVaultEntries,
  localDateString,
  matchesVaultQuery,
  reapplyVaultFilter,
  removeVaultEntry,
  replaceVaultEntry,
  vaultEmptyStateText,
  vaultHeaderText,
  vaultHaystack,
  type VaultSearchable,
} from "./vault-search";

/** 造一条参与匹配的条目（只填搜索用得到的字段）。 */
function entry(over: Partial<VaultSearchable> & { id: number }): VaultSearchable {
  return {
    note: "",
    auto_category: "text",
    tags: [],
    created_at: 1_700_000_000,
    ...over,
  };
}

/** 本地正午构造的时间戳 → 它的本地日期必然是那一天（与时区无关）。 */
function localNoon(year: number, month: number, day: number): number {
  return new Date(year, month - 1, day, 12, 0, 0).getTime() / 1000;
}

describe("查询拆词与匹配", () => {
  it("空查询命中一切", () => {
    const e = entry({ id: 1, note: "服务器密码" });
    expect(matchesVaultQuery(e, "")).toBe(true);
    expect(matchesVaultQuery(e, "   ")).toBe(true);
    expect(filterVaultEntries([e], "")).toEqual([e]);
  });

  it("按备注命中", () => {
    const e = entry({ id: 1, note: "服务器密码" });
    expect(matchesVaultQuery(e, "密码")).toBe(true);
    expect(matchesVaultQuery(e, "服务器")).toBe(true);
    expect(matchesVaultQuery(e, "信用卡")).toBe(false);
  });

  it("按标签命中（任一标签命中即可）", () => {
    const e = entry({ id: 1, tags: ["work", "ssh"] });
    expect(matchesVaultQuery(e, "ssh")).toBe(true);
    expect(matchesVaultQuery(e, "work")).toBe(true);
    expect(matchesVaultQuery(e, "personal")).toBe(false);
  });

  it("按类别命中", () => {
    const e = entry({ id: 1, auto_category: "image" });
    expect(matchesVaultQuery(e, "image")).toBe(true);
    expect(matchesVaultQuery(e, "text")).toBe(false);
  });

  it("按本地日期命中，且 MM-DD 也命中（YYYY-MM-DD 的子串）", () => {
    const e = entry({ id: 1, created_at: localNoon(2026, 10, 16) });
    expect(matchesVaultQuery(e, "2026-10-16")).toBe(true);
    expect(matchesVaultQuery(e, "10-16")).toBe(true);
    expect(matchesVaultQuery(e, "2026")).toBe(true);
    expect(matchesVaultQuery(e, "2026-10-17")).toBe(false);
  });

  it("本地日期取本地时区（不是 UTC）", () => {
    // 用本地正午构造：无论机器在哪个时区，本地日期都必须是那一天。
    // UTC 实现会在这里差一天（UTC+8 的机器上会返回 10-15）。
    expect(localDateString(localNoon(2026, 10, 16))).toBe("2026-10-16");
  });

  it("词间是「与」：两个词都命中才算命中", () => {
    const e = entry({ id: 1, note: "服务器密码" });
    expect(matchesVaultQuery(e, "服务器 密码")).toBe(true);
    expect(matchesVaultQuery(e, "服务器 WiFi")).toBe(false);
  });

  it("大小写不敏感", () => {
    const e = entry({ id: 1, tags: ["Client-Acme"] });
    expect(matchesVaultQuery(e, "acme")).toBe(true);
    expect(matchesVaultQuery(e, "ACME")).toBe(true);
    expect(matchesVaultQuery(e, "client")).toBe(true);
  });

  it("不跨字段误命中（备注尾 + 标签头不算连着）", () => {
    const e = entry({ id: 1, note: "ab", tags: ["c"] });
    expect(matchesVaultQuery(e, "abc")).toBe(false);
    expect(matchesVaultQuery(e, "ab")).toBe(true);
    expect(matchesVaultQuery(e, "c")).toBe(true);
  });

  it("可搜索文本只含备注 / 类别 / 标签 / 日期这四项", () => {
    const e = entry({
      id: 1,
      note: "n",
      auto_category: "c",
      tags: ["t"],
      created_at: localNoon(2026, 10, 16),
    });
    const hay = vaultHaystack(e);
    expect(hay).toContain("n");
    expect(hay).toContain("c");
    expect(hay).toContain("t");
    expect(hay).toContain("2026-10-16");
  });
});

describe("过滤结果", () => {
  const list = [
    entry({ id: 1, note: "服务器密码" }),
    entry({ id: 2, note: "WiFi" }),
    entry({ id: 3, note: "信用卡" }),
  ];

  it("过滤出命中项，顺序与输入一致", () => {
    // 「密」只出现在第 1 条的备注里；第 3 条是「信用卡」（有「信」没「密」）。
    expect(filterVaultEntries(list, "密").map((e) => e.id)).toEqual([1]);
    expect(filterVaultEntries(list, "信").map((e) => e.id)).toEqual([3]);
  });

  it("空查询返回全量的一份拷贝（不与全量共享引用）", () => {
    const view = filterVaultEntries(list, "");
    expect(view).toEqual(list);
    expect(view).not.toBe(list);
  });
});

describe("选中收敛：两条路径不同", () => {
  const list = [
    entry({ id: 1, note: "服务器密码" }),
    entry({ id: 2, note: "WiFi 密码" }),
    entry({ id: 3, note: "信用卡" }),
  ];

  it("查询变化 → 选中回到第一条", () => {
    const { view, selected } = applyVaultQuery(list, "密码");
    expect(view.map((e) => e.id)).toEqual([1, 2]);
    expect(selected).toBe(0);
  });

  it("数据变化 → 该条目仍在结果里就保持选中（不跳回第一条）", () => {
    // 改完备注仍命中查询：选中仍是它，不是第一条。
    const { view, selected } = reapplyVaultFilter(list, "密码", { id: 2, index: 1 });
    expect(view.map((e) => e.id)).toEqual([1, 2]);
    expect(selected).toBe(1);
  });

  it("数据变化 → 该条目已不在结果里就按旧索引夹紧", () => {
    // 第 3 条被删掉：视图只剩两条，旧索引 2 夹到 1。
    const after = removeVaultEntry(list, 3);
    const { view, selected } = reapplyVaultFilter(after, "", { id: 3, index: 2 });
    expect(view.map((e) => e.id)).toEqual([1, 2]);
    expect(selected).toBe(1);
  });

  it("数据变化 → 视图空了也不越界", () => {
    const { view, selected } = reapplyVaultFilter([], "", { id: 3, index: 2 });
    expect(view).toEqual([]);
    expect(selected).toBe(0);
  });

  it("按 id 摘行 / 换行，其余条目不动", () => {
    expect(removeVaultEntry(list, 2).map((e) => e.id)).toEqual([1, 3]);
    const updated = { ...list[1], note: "新备注" };
    const replaced = replaceVaultEntry(list, updated);
    expect(replaced[1].note).toBe("新备注");
    expect(replaced[0]).toBe(list[0]);
    expect(replaced[2]).toBe(list[2]);
  });
});

describe("文案", () => {
  it("计数行：总数 / 无法解密 / 匹配", () => {
    expect(vaultHeaderText(12, 0, 12, false)).toBe("共 12 条");
    expect(vaultHeaderText(12, 3, 12, false)).toBe("共 12 条 · 3 条无法解密");
    expect(vaultHeaderText(12, 3, 2, true)).toBe("共 12 条 · 3 条无法解密 · 匹配 2 条");
  });

  it("总数为 0 → 空串（引导语含平台键位，留在 main.ts）", () => {
    expect(vaultHeaderText(0, 0, 0, false)).toBe("");
    expect(vaultHeaderText(0, 0, 0, true)).toBe("");
  });

  it("空态文案区分「没条目」与「没命中」", () => {
    expect(vaultEmptyStateText(false)).toBe("加密区暂无条目");
    expect(vaultEmptyStateText(true)).toBe("没有匹配的条目");
  });
});
