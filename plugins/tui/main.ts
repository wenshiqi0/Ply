import plugins from "wendy:plugins";
import * as commands from "wendy:commands";
import * as terminal from "wendy:terminal";
import * as markdown from "wendy:markdown";
import * as events from "wendy:events";
import { onDispose } from "wendy:lifecycle";

type Decision = WendyApprovalResponse;
type Review = { request: WendyApprovalRequest | WendyToolApprovalRequest; resolve: (decision: Decision) => void; cancelled: boolean; offset: number; seenEnd: boolean; lines?: string[]; columns?: number };
type Outcome = { output?: any; error?: string };
type HistoryEntry = { text: string; role: "user" | "assistant" | "system" | "error"; document?: markdown.Node };
type InputRow = { text: string; start: number; end: number; cells: number };
type Session = {
  id: number; fiber: string; columns: number; rows: number; dirty: boolean; model: string;
  approvalMode: "ask" | "risk" | "bypass";
  reviews: Review[]; history: HistoryEntry[]; input: string; caret: number; inputView: number; offset: number; busy: boolean; outcome?: Outcome;
  sent: string[]; recalled: number; draft: string; goalColumn?: number; notice: string;
  screenKey: string; startedAt: number; lastPulse: number;
  bodyHeight: number;
  historyLines?: terminal.Line[]; historyColumns?: number;
  pendingKeys: string; paste: boolean; lastKey: number; approved: number; rejected: number; tasks: number;
  inputTooLong: boolean;
  calls: number; inputTokens: number; outputTokens: number; cacheRead: number | null; usageComplete: boolean;
};
let active: Session | undefined;
const segmenter = typeof Intl !== "undefined" && typeof Intl.Segmenter === "function" ? new Intl.Segmenter(undefined, { granularity: "grapheme" }) : null;
const chars = (text: string): string[] => segmenter ? Array.from(segmenter.segment(text), item => item.segment) : Array.from(text);

function clean(text: string): string {
  return text.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g,
    c => "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0")).replace(/\t/g, "    ");
}
function width(c: string): number {
  if (/^[\p{Mark}\u200d\ufe0f]+$/u.test(c)) return 0;
  const n = c.codePointAt(0)!;
  if (c.includes("\ufe0f") || c.includes("\u200d") || n >= 0x1f1e6 && n <= 0x1f1ff) return 2;
  return n >= 0x1100 && (n <= 0x115f || n >= 0x2e80 && n <= 0xa4cf || n >= 0xac00 && n <= 0xd7a3 || n >= 0xf900 && n <= 0xfaff || n >= 0xfe10 && n <= 0xfe6f || n >= 0xff00 && n <= 0xff60 || n >= 0xffe0 && n <= 0xffe6 || n >= 0x1f300) ? 2 : 1;
}
function wrap(text: string, columns: number): string[] {
  const result: string[] = [];
  for (const line of clean(text).split("\n")) {
    let part = "", used = 0;
    for (const c of chars(line)) {
      const size = width(c);
      if (used + size > columns) { result.push(part); part = ""; used = 0; }
      part += c; used += size;
    }
    result.push(part);
  }
  return result;
}
function oneLine(text: string, columns: number): string { return wrap(text.replace(/\n/g, " "), columns)[0] ?? ""; }
function pad(text: string, columns: number): string {
  const fitted = oneLine(text, columns);
  return fitted + " ".repeat(Math.max(0, columns - chars(fitted).reduce((sum, c) => sum + width(c), 0)));
}
function add(s: Session, text: string, role: HistoryEntry["role"] = "system") {
  s.history.push({ text, role });
  s.historyLines = undefined;
  // Reports are also available in --record; approval requests are never cut.
  if (s.history.length > 200) s.history.splice(0, s.history.length - 200);
  s.offset = Number.MAX_SAFE_INTEGER; s.dirty = true;
}

function inputLayout(text: string, columns: number, caret: number) {
  const value = chars(text), rows: InputRow[] = [{ text: "", start: 0, end: 0, cells: 0 }];
  for (let i = 0; i < value.length; i++) {
    let row = rows[rows.length - 1]; const c = value[i];
    if (c === "\n") { row.end = i; rows.push({ text: "", start: i + 1, end: i + 1, cells: 0 }); continue; }
    if (row.cells + width(c) > columns) { row.end = i; row = { text: "", start: i, end: i, cells: 0 }; rows.push(row); }
    row.text += c; row.cells += width(c); row.end = i + 1;
    if (row.cells === columns && value[i + 1] !== "\n") rows.push({ text: "", start: i + 1, end: i + 1, cells: 0 });
  }
  let row = 0;
  for (let i = 0; i < rows.length; i++) if (rows[i].start <= caret) row = i;
  const column = value.slice(rows[row].start, caret).reduce((sum, c) => sum + width(c), 0);
  return { rows, row, column };
}
function insert(s: Session, text: string) {
  text = clean(text);
  const value = chars(s.input), prefix = value.slice(0, s.caret).join("");
  const next = prefix + text + value.slice(s.caret).join("");
  if (next.length > 65536) { s.inputTooLong = true; s.notice = "输入超过 65536 字符；这段内容未插入，Ctrl+U 清空后重试"; return; }
  s.input = next; s.caret = chars(prefix + text).length; s.goalColumn = undefined; s.notice = "";
}
function remove(s: Session, from: number, to: number) {
  const value = chars(s.input);
  s.input = value.slice(0, from).join("") + value.slice(to).join(""); s.caret = from; s.goalColumn = undefined;
}
function lineBounds(s: Session) {
  const value = chars(s.input); let start = s.caret, end = s.caret;
  while (start > 0 && value[start - 1] !== "\n") start--;
  while (end < value.length && value[end] !== "\n") end++;
  return { start, end };
}
function recall(s: Session, direction: number) {
  if (!s.sent.length) return;
  if (s.recalled === s.sent.length) s.draft = s.input;
  s.recalled = Math.max(0, Math.min(s.sent.length, s.recalled + direction));
  s.input = s.recalled === s.sent.length ? s.draft : s.sent[s.recalled];
  s.caret = chars(s.input).length; s.goalColumn = undefined; s.notice = "";
}
function vertical(s: Session, direction: number) {
  const layout = inputLayout(s.input, Math.max(1, s.columns - 7), s.caret), target = layout.row + direction;
  if (target < 0 || target >= layout.rows.length) { recall(s, direction); return; }
  const goal = s.goalColumn ?? layout.column, row = layout.rows[target], value = chars(s.input);
  let i = row.start, column = 0;
  while (i < row.end && column + width(value[i]) <= goal) column += width(value[i++]);
  s.caret = i; s.goalColumn = goal;
}
function edit(s: Session, key: string, paste?: boolean) {
  const count = chars(s.input).length;
  if (paste) { insert(s, clean(key.replace(/\r\n?/g, "\n"))); return; }
  if (key === "left" || key === "\x02") s.caret = Math.max(0, s.caret - 1);
  else if (key === "right" || key === "\x06") s.caret = Math.min(count, s.caret + 1);
  else if (key === "home" || key === "\x01") s.caret = lineBounds(s).start;
  else if (key === "end" || key === "\x05") s.caret = lineBounds(s).end;
  else if (key === "up" || key === "\x10") { vertical(s, -1); return; }
  else if (key === "down" || key === "\x0e") { vertical(s, 1); return; }
  else if (key === "backspace" && s.caret > 0) remove(s, s.caret - 1, s.caret);
  else if (key === "delete" || key === "eof") remove(s, s.caret, Math.min(count, s.caret + 1));
  else if (key === "\x15") { remove(s, lineBounds(s).start, s.caret); s.inputTooLong = false; s.notice = ""; }
  else if (key === "\x0b") remove(s, s.caret, lineBounds(s).end);
  else if (key === "\x17") {
    const value = chars(s.input); let start = s.caret;
    while (start > 0 && /\s/u.test(value[start - 1])) start--;
    while (start > 0 && !/\s/u.test(value[start - 1])) start--;
    remove(s, start, s.caret);
  } else if (key === "newline") insert(s, "\n");
  else if (!['ignored', 'escape', 'enter'].includes(key) && !/[\x00-\x1f]/.test(key)) insert(s, key);
  s.goalColumn = undefined;
}
function reviewText(review: Review): string {
  const r = review.request;
  const assessments = ("assessments" in r ? r.assessments : []).map((value: any, index) => JSON.stringify({
    check: index + 1, decision: value?.decision, reason: value?.reason,
    reason_codes: value?.reason_codes, risk_probabilities: value?.risk_probabilities,
  }, null, 2));
  return [`工具: ${r.tool}`, `目录: ${r.cwd}`,
    "", ...("command" in r ? ["原始命令（仅本次授权，不改写命令）：", r.command]
      : ["原始调用参数（读取或写入前审批，仅本次授权）：", JSON.stringify(r.input, null, 2)]),
    ...(assessments.length ? ["", "风险评估：", ...assessments] : [])].join("\n");
}
function transcript(s: Session, columns: number): terminal.Line[] {
  if (s.historyLines && s.historyColumns === columns) return s.historyLines;
  const lines: terminal.Line[] = [];
  if (!s.history.length) {
    lines.push({ text: "开始对话", style: "title" }, "");
    for (const line of wrap("在下方输入任务，按 Enter 发送。\n支持多行粘贴；Alt+Enter 或 Ctrl+J 换行。", columns)) lines.push({ text: line, style: "muted" });
    for (const line of wrap(s.approvalMode === "bypass" ? "BYPASS 已启用：工具调用跳过人工确认。" : s.approvalMode === "ask" ? "读取、写入与执行操作会先请求确认。" : "工具调用按风险策略请求确认。", columns)) lines.push({ text: line, style: s.approvalMode === "bypass" ? "warning" : "muted" });
  }
  for (const entry of s.history) {
    if (lines.length) lines.push("");
    if (entry.role === "user" || entry.role === "assistant") lines.push({ text: entry.role === "user" ? "你 ›" : "Wendy ›", style: entry.role === "user" ? "accent" : "title" });
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
function topBorder(label: string, columns: number): string {
  const text = "╭─ " + label + " ";
  return text + "─".repeat(Math.max(0, columns - chars(text).reduce((n,c) => n + width(c),0) - 1)) + "╮";
}
function paint(s: Session) {
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
    lines.push({ text: oneLine("Wendy" + (s.approvalMode === "bypass" ? " · BYPASS" : "") + " · " + s.model, columns), style: s.approvalMode === "bypass" ? "warning" : "title" },
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
function keys(s: Session, data: string): { key: string; paste?: boolean }[] {
  s.pendingKeys += data;
  if (data) s.lastKey = Date.now();
  const result: { key: string; paste?: boolean }[] = [];
  const names: Record<string, string> = { "\x1b[A": "up", "\x1b[B": "down", "\x1b[C": "right", "\x1b[D": "left", "\x1b[5~": "pageup", "\x1b[6~": "pagedown", "\x1b[H": "home", "\x1b[F": "end", "\x1b[1~": "home", "\x1b[4~": "end", "\x1b[3~": "delete", "\x1b[13;2u": "newline", "\x1b[27;2;13~": "newline", "\x1bOA": "up", "\x1bOB": "down", "\x1bOC": "right", "\x1bOD": "left", "\x1bOH": "home", "\x1bOF": "end" };
  while (s.pendingKeys) {
    if (s.paste) {
      const end = s.pendingKeys.indexOf("\x1b[201~");
      if (end < 0) { if (s.pendingKeys.length > 65536) { s.inputTooLong = true; s.pendingKeys = s.pendingKeys.slice(-6); } break; }
      result.push({ key: s.pendingKeys.slice(0, end), paste: true });
      s.pendingKeys = s.pendingKeys.slice(end + 6); s.paste = false; continue;
    }
    if (s.pendingKeys.startsWith("\x1b[200~")) { s.paste = true; s.pendingKeys = s.pendingKeys.slice(6); continue; }
    if (s.pendingKeys.startsWith("\x1b")) {
      if (s.pendingKeys.startsWith("\x1b\r") || s.pendingKeys.startsWith("\x1b\n")) { result.push({ key: "newline" }); s.pendingKeys = s.pendingKeys.slice(2); continue; }
      const sequence = /^(?:\x1b\[[0-?]*[ -/]*[@-~]|\x1bO[A-DHF])/.exec(s.pendingKeys)?.[0];
      if (sequence) { result.push({ key: names[sequence] ?? "ignored" }); s.pendingKeys = s.pendingKeys.slice(sequence.length); continue; }
      if (Date.now() - s.lastKey < 150) break;
      result.push({ key: "escape" }); s.pendingKeys = s.pendingKeys.slice(1); continue;
    }
    const c = String.fromCodePoint(s.pendingKeys.codePointAt(0)!);
    s.pendingKeys = s.pendingKeys.slice(c.length);
    result.push({ key: c === "\r" ? "enter" : c === "\n" ? "newline" : c === "\x03" ? "interrupt" : c === "\x04" ? "eof" : c === "\x7f" || c === "\b" ? "backspace" : c });
  }
  return result;
}
function usage(s: Session, value: any) {
  if (!value) return;
  s.calls += value.calls ?? 0; s.inputTokens += value.input_tokens ?? 0; s.outputTokens += value.output_tokens ?? 0;
  s.cacheRead = s.cacheRead === null || value.cache_read_tokens === null || value.cache_read_tokens === undefined ? null : s.cacheRead + value.cache_read_tokens;
  s.usageComplete = s.usageComplete && value.complete === true;
  const rate = value.cache_hit_rate == null ? "未知" : (value.cache_hit_rate * 100).toFixed(1) + "%";
  add(s, `本任务用量：调用 ${value.calls} · 输入 ${value.input_tokens} · 输出 ${value.output_tokens} · 缓存命中 ${rate}${value.complete ? "" : " · 用量不完整"}`);
}
function launch(s: Session, text: string) {
  s.busy = true; s.outcome = undefined; s.startedAt = Date.now(); s.lastPulse = 0; s.notice = ""; add(s, text, "user");
  const operation = plugins.agent({ prompt: text });
  // The TUI fiber owns this await, including each agent/approval descendant.
  Promise.resolve(operation).then(output => { s.outcome = { output }; s.dirty = true; }, error => { s.outcome = { error: String(error) }; s.dirty = true; });
}
function finish(s: Session) {
  const outcome = s.outcome!; s.outcome = undefined; s.busy = false;
  s.notice = "";
  if (outcome.error) { s.usageComplete = false; add(s, "任务失败：" + outcome.error + "\n本任务用量请查 --record；会话小计只包含已返回的 agent 用量。", "error"); return; }
  const output = outcome.output;
  if (output?.status === "approval_required") add(s, "任务待审批，尚未完成：\n" + JSON.stringify(output.approval_requests ?? output.approval, null, 2));
  else { s.tasks++; add(s, output?.text ?? output?.content ?? (output?.stdout !== undefined ? output.stdout + output.stderr + `\n退出码：${output.exit_code}` : JSON.stringify(output, null, 2)), "assistant"); }
  usage(s, output?.total_usage);
}

/** Interactive terminal task console; use `wendy tui`. Not registered as a model tool. */
export async function onHandle(input: { prompt?: string }): Promise<{ tasks: number; approved: number; rejected: number; usage: unknown }> {
  if (active) throw new Error("a TUI session is already active");
  if (input?.prompt !== undefined && (typeof input.prompt !== "string" || !input.prompt.trim())) throw new Error("prompt must be a non-empty string");
  const current = await fiber.info();
  const profile = typeof ctx.currentModel === "function" ? await ctx.currentModel() : null;
  const approval = typeof ctx.approvalStatus === "function" ? await ctx.approvalStatus() : null;
  const opened = terminal.open();
  const s: Session = { ...opened, fiber: current.id, model: profile?.model ?? "未选择模型", dirty: true, reviews: [], history: [], input: "", caret: 0, inputView: 0, offset: 0, busy: false,
    approvalMode: approval?.mode ?? "risk",
    sent: [], recalled: 0, draft: "", notice: "", screenKey: "", startedAt: 0, lastPulse: 0, bodyHeight: 1,
    pendingKeys: "", paste: false, lastKey: 0, approved: 0, rejected: 0, tasks: 0, inputTooLong: false,
    calls: 0, inputTokens: 0, outputTokens: 0, cacheRead: 0, usageComplete: true };
  active = s;
  const close = () => {
    active = undefined;
    for (const r of s.reviews.splice(0)) { r.cancelled = true; r.resolve({ request_id: r.request.id, decision: "pending", reason: "terminal closed" }); }
    try { terminal.close(s.id); } catch {}
  };
  const unwatch = onDispose(close);
  try {
    if (input?.prompt) launch(s, input.prompt);
    for (;;) {
      s.reviews = s.reviews.filter(r => !r.cancelled);
      if (s.outcome) finish(s);
      if (s.busy && Date.now() - s.lastPulse >= 160) { s.lastPulse = Date.now(); s.dirty = true; }
      if (s.dirty) paint(s);
      const event = await terminal.read(s.id, 100);
      if (event.columns !== s.columns || event.rows !== s.rows) { s.columns = event.columns; s.rows = event.rows; s.dirty = true; }
      let quit = false;
      for (const { key, paste } of keys(s, event.data)) {
        if (key === "interrupt" || key === "eof" && !s.input) { if (s.busy) throw new Error("TUI interrupted by user; active task cancelled"); quit = true; break; }
        if (s.columns < 40 || s.rows < 10) continue;
        if (key === "\x0c") { s.screenKey = ""; s.dirty = true; continue; }
        const r = s.reviews[0];
        if (r) {
          if (paste) continue;
          const step = Math.max(1, s.rows - 7);
          if (["down", "j"].includes(key)) r.offset++;
          if (["up", "k"].includes(key)) r.offset = Math.max(0, r.offset - 1);
          if (["pagedown", " "].includes(key)) r.offset += step;
          if (key === "pageup") r.offset = Math.max(0, r.offset - step);
          if (key === "y" && r.seenEnd || ["n", "enter", "escape"].includes(key)) {
            const approved = key === "y";
            if (approved) s.approved++; else s.rejected++;
            s.reviews.shift();
            r.resolve({ request_id: r.request.id, decision: approved ? "approved" : "rejected", approver: "local-terminal", reference: "tty:" + r.request.id, reason: approved ? "user approved this operation in TUI" : "user rejected this operation in TUI" });
            add(s, `${approved ? "已批准" : "已拒绝"}本次 ${r.request.tool} 操作`);
            terminal.flush(s.id); s.pendingKeys = "";
            // Do not apply leftover keystrokes to a different request.
            s.dirty = true; break;
          }
          s.dirty = true; continue;
        }
        if (["pageup", "pagedown"].includes(key)) {
          s.offset = Math.max(0, s.offset + (key === "pageup" ? -1 : 1) * s.bodyHeight);
          s.dirty = true; continue;
        }
        if (key === "enter" && !paste) {
          s.dirty = true;
          if (s.busy) { s.notice = "执行中 · 草稿已保留，任务完成后按 Enter 发送"; continue; }
          const text = s.input, command = text.trim();
          if (s.inputTooLong) { s.inputTooLong = false; s.input = ""; s.caret = 0; s.notice = "输入超限，未提交；请重新输入较短消息。"; continue; }
          s.input = ""; s.caret = 0; s.inputView = 0; s.notice = "";
          if (command === "/quit" || command === "/exit") { quit = true; break; }
          if (command === "/clear") { s.history = []; s.historyLines = undefined; s.offset = 0; }
          else if (command === "/help") add(s, "输入消息交给 agent，Enter 发送。\nAlt+Enter / Ctrl+J：换行；粘贴保留多行。\n←→、Home/End、Ctrl+A/E：移动光标。\n↑↓：在输入行之间移动，首末行切换历史消息。\nBackspace/Delete：删除；Ctrl+U/K/W：删除到行首/行末/前一词。\nPageUp/PageDown：翻阅结果；Ctrl+L：重绘。\n/clear：清屏；/quit 或 Ctrl+C：退出并取消任务。\n审批 y 批准本次，n/Enter/Esc 拒绝，↑↓/Space 滚动。只有当前终端的明确输入才能批准。");
          else if (command) {
            if (s.sent[s.sent.length - 1] !== text) s.sent.push(text);
            if (s.sent.length > 100) s.sent.shift(); s.recalled = s.sent.length; s.draft = "";
            launch(s, text);
          }
        } else edit(s, key, paste);
        s.dirty = true;
      }
      if (quit) break;
    }
    const result = { tasks: s.tasks, approved: s.approved, rejected: s.rejected, usage: { calls: s.calls, input_tokens: s.inputTokens, output_tokens: s.outputTokens,
      cache_read_tokens: s.cacheRead, cache_hit_rate: s.cacheRead === null || s.inputTokens === 0 ? null : s.cacheRead / s.inputTokens, complete: s.usageComplete } };
    await fiber.record({ type: "tui_session", ...result });
    return result;
  } finally { close(); unwatch(); }
}

/** Environment adapter only; never adds an approval or TUI tool to the model. */
export async function onLoad() {
  await plugins.approval.loaded;
  await plugins.agent.loaded;
  const review = async (event: events.Event<WendyApprovalRequest | WendyToolApprovalRequest>): Promise<Decision> => {
    const s = active;
    if (!s || !(await fiber.info()).ancestors.includes(s.fiber)) return { request_id: event.data.id, decision: "pending", reason: "no owning TUI session" };
    let review!: Review;
    const answer = new Promise<Decision>(resolve => { review = { request: event.data, resolve, cancelled: false, offset: 0, seenEnd: false }; });
    const unwatch = onDispose(() => { review.cancelled = true; s.dirty = true; });
    try {
      // A fresh review must never consume keys buffered for task input.
      terminal.flush(s.id); s.pendingKeys = ""; s.paste = false;
      s.reviews.push(review); s.dirty = true;
      const decision = await answer;
      await fiber.record({ type: "tui_approval", request_id: event.data.id, decision: decision.decision });
      return decision;
    } finally { review.cancelled = true; s.dirty = true; unwatch(); }
  };
  await events.on("approval.request", review);
  await events.on("approval.toolRequest", review);
  await commands.register({ name: "tui", description: "与 agent 交互并处理工具审批", handler: onHandle });
}
