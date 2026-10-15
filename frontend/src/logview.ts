// ClipVault 设置面板的「诊断日志」区（U4）。
//
// 设置区只放 render 与 IPC 包装，hold 状态由 main.ts 驱动。`formatTailForView`
// 是纯函数：把后端 cap 过的尾部再裁一次到「最多 200 行」展示（KTD7），
// 超过截断时补一行省略提示。

/** 单次展示的最大行数——后端 R13 cap 在 64 KiB / 400 行,前端再 cap 一道(KTD7)。 */
export const MAX_VIEW_LINES = 200;

/** 把后端 cap 后的尾部字符串再裁一次。空串 → 空串;行数不足 maxLines → 原文;过多 → 末段 + 省略提示。 */
export function formatTailForView(raw: string, maxLines: number = MAX_VIEW_LINES): string {
  if (raw === "") return "";
  const all = raw.split("\n");
  // split("\n") 在末尾 "\n" 处会多出一个空段,过滤掉。
  const lines = all.filter((l) => l.length > 0);
  if (lines.length <= maxLines) return lines.join("\n") + "\n";
  const tail = lines.slice(-maxLines);
  const dropped = lines.length - maxLines;
  return `…（已省略前 ${dropped} 行）\n` + tail.join("\n") + "\n";
}

/** 字节数 → 人类可读(KiB / MiB)。诊断区右侧小字用。 */
export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MiB`;
}
