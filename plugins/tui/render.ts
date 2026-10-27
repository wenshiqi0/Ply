import * as markdown from "ply:markdown";
import * as terminal from "ply:terminal";
import type { HistoryEntry, Review, Session } from "./types";
import { chars, oneLine, pad, width, wrap } from "./text";
import { inputLayout } from "./composer";

export function add(s: Session, text: string, role: HistoryEntry["role"] = "system") {
  s.history.push({ text, role });
  s.historyLines = undefined;
  // Reports are also available in --record; approval requests are never cut.
  if (s.history.length > 200) s.history.splice(0, s.history.length - 200);
  s.offset = Number.MAX_SAFE_INTEGER; s.dirty = true;
}

export function reviewText(review: Review): string {
  const r = review.request;
  const assessments = ("assessments" in r ? r.assessments : []).map((raw: unknown, index) => {
    const value = raw && typeof raw === "object" ? raw as Record<string, unknown> : {};
    return JSON.stringify({
    check: index + 1, decision: value?.decision, reason: value?.reason,
    reason_codes: value?.reason_codes, risk_probabilities: value?.risk_probabilities,
  }, null, 2); });
  return [`工具: ${r.tool}`, `目录: ${r.cwd}`,
    "", ...("command" in r ? ["原始命令（仅本次授权，不改写命令）：", r.command]
      : ["原始调用参数（读取或写入前审批，仅本次授权）：", JSON.stringify(r.input, null, 2)]),
    ...(assessments.length ? ["", "风险评估：", ...assessments] : [])].join("\n");
}

export function transcript(s: Session, columns: number): terminal.Line[] {
  if (s.historyLines && s.historyColumns === columns) return s.historyLines;
  const lines: terminal.Line[] = [];
  if (!s.history.length) {
    lines.push({ text: "开始对话", style: "title" }, "");
    for (const line of wrap("在下方输入任务，按 Enter 发送。\n支持多行粘贴；Alt+Enter 或 Ctrl+J 换行。", columns)) lines.push({ text: line, style: "muted" });
    for (const line of wrap(s.approvalMode === "bypass" ? "BYPASS 已启用：工具调用跳过人工确认。" : s.approvalMode === "ask" ? "读取、写入与执行操作会先请求确认。" : "工具调用按风险策略请求确认。", columns)) lines.push({ text: line, style: s.approvalMode === "bypass" ? "warning" : "muted" });
  }
  for (const entry of s.history) {
    if (lines.length) lines.push("");
    if (entry.role === "user" || entry.role === "assistant") lines.push({ text: entry.role === "user" ? "你 ›" : "Ply ›", style: entry.role === "user" ? "accent" : "title" });
    if (entry.role === "assistant") {
      try {
        entry.document ??= markdown.parse(entry.text);
        lines.push(...markdown.render(entry.document, { columns }));
      } catch {
        lines.push({ text: "格式化失败，以下按文本显示：", style: "muted" });
        for (const line of wrap(entry.text, columns)) lines.push({ text: line });
      }
    } else for (const line of wrap(entry.text, columns)) lines.push({ text: line, style: entry.role === "error" ? "error" : entry.role === "system" ? "muted" : "normal" });
  }
  s.historyLines = lines; s.historyColumns = columns; return lines;
}

export function topBorder(label: string, columns: number): string {
  const text = "╭─ " + label + " ";
  return text + "─".repeat(Math.max(0, columns - chars(text).reduce((n,c) => n + width(c),0) - 1)) + "╮";
}

export function paint(s: Session) {
  if (s.columns < 40 || s.rows < 10) {
    terminal.render(s.id, [oneLine("终端过小：至少需要 40×10；Ctrl-C 退出", Math.max(1, s.columns - 1))], { clear: s.screenKey !== "small" });
    s.screenKey = "small";
    s.dirty = false; return;
  }
  const columns = s.columns - 1;
  const r = s.reviews[0];
  const key = r ? "approval:" + r.request.id : s.busy ? "busy" : "composer";
  const lines: terminal.Line[] = [];
  let cursor: { row: number; column: number } | null = null;
  if (r) {
    const height = s.rows - 7;
    if (!r.lines || r.columns !== columns) { r.lines = wrap(reviewText(r), columns); r.columns = columns; }
    r.offset = Math.min(r.offset, Math.max(0, r.lines.length - height));
    if (r.offset + height >= r.lines.length) r.seenEnd = true;
    lines.push({ text: oneLine(`需要你的确认 · ${s.reviews.length} 个请求`, columns), style: "warning" },
      { text: oneLine("只允许当前这次操作；你的输入草稿会保留。", columns), style: "muted" },
      { text: oneLine(`第 ${r.offset + 1}–${Math.min(r.lines.length, r.offset + height)} / ${r.lines.length} 行`, columns), style: "muted" });
    lines.push(...r.lines.slice(r.offset, r.offset + height));
    while (lines.length < s.rows - 4) lines.push("");
    lines.push({ text: topBorder("审批选择", columns), style: "warning" },
      { text: "│ " + pad(r.seenEnd ? "[y] 批准本次  [n/Enter/Esc] 拒绝" : "先滚动查看完整请求，再按 y 批准；n 拒绝", columns - 4) + " │", style: "warning" },
      { text: "│ " + pad("↑↓ / Space 翻页 · Ctrl+C 取消并退出", columns - 4) + " │", style: "muted" },
      { text: "╰" + "─".repeat(columns - 2) + "╯", style: "warning" });
  } else {
    const inputWidth = columns - 6, layout = inputLayout(s.input, inputWidth, s.caret);
    const shown = Math.min(Math.min(6, s.rows - 8), Math.max(2, layout.rows.length));
    s.inputView = Math.max(0, Math.min(s.inputView, Math.max(0, layout.rows.length - shown)));
    if (layout.row < s.inputView) s.inputView = layout.row;
    if (layout.row >= s.inputView + shown) s.inputView = layout.row - shown + 1;
    const height = s.rows - shown - 7, content = transcript(s, columns);
    s.bodyHeight = height;
    s.offset = Math.min(s.offset, Math.max(0, content.length - height));
    lines.push({ text: oneLine("Ply" + (s.approvalMode === "bypass" ? " · BYPASS" : "") + " · " + s.model, columns), style: s.approvalMode === "bypass" ? "warning" : "title" },
      { text: oneLine("工作目录 " + process.cwd(), columns), style: "muted" }, "");
    lines.push(...content.slice(s.offset, s.offset + height));
    while (lines.length < 3 + height) lines.push("");
    const elapsed = Math.floor((Date.now() - s.startedAt) / 1000);
    const pulse = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"][Math.floor(Date.now() / 160) % 8];
    const status = s.notice || (s.busy ? `${pulse} 执行中 · ${elapsed}s · 可编辑下一条草稿，完成后发送` : s.approvalMode === "bypass" ? "就绪 · BYPASS：跳过人工确认" : s.approvalMode === "ask" ? "就绪 · 工具操作会先请求确认" : "就绪 · 按风险策略请求确认");
    lines.push({ text: oneLine(status, columns), style: s.busy ? "warning" : "muted" });
    const label = s.busy ? "下一条消息 · 草稿保留" : layout.rows.length > 2 ? `输入消息 · ${layout.row + 1}/${layout.rows.length} 行` : "输入消息";
    lines.push({ text: topBorder(label, columns), style: "accent" });
    const inputStart = lines.length;
    for (let i = 0; i < shown; i++) {
      const value = layout.rows[s.inputView + i]?.text ?? "";
      const placeholder = !s.input && i === 0;
      lines.push({ text: (i === 0 && s.inputView === 0 ? "│ › " : "│   ") + pad(placeholder ? "输入消息，按 Enter 发送…" : value, inputWidth) + " │", style: placeholder ? "muted" : "normal" });
    }
    lines.push({ text: "╰" + "─".repeat(columns - 2) + "╯", style: "accent" },
      { text: oneLine("Enter 发送 · Ctrl+J 换行 · ↑↓ 历史 · Ctrl+C 退出 · /help", columns), style: "muted" });
    cursor = { row: inputStart + layout.row - s.inputView, column: 4 + layout.column };
  }
  const size = terminal.render(s.id, lines, { cursor, clear: s.screenKey !== key });
  s.screenKey = key;
  s.columns = size.columns; s.rows = size.rows; s.dirty = false;
}
