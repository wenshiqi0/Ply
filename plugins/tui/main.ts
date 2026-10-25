import plugins from "wendy:plugins";
import * as commands from "wendy:commands";
import * as terminal from "wendy:terminal";
import * as events from "wendy:events";
import { onDispose } from "wendy:lifecycle";

type Decision = WendyApprovalResponse;
type Review = { request: WendyApprovalRequest | WendyToolApprovalRequest; resolve: (decision: Decision) => void; cancelled: boolean; offset: number; seenEnd: boolean };
type Outcome = { output?: any; error?: string };
type Session = {
  id: number; fiber: string; columns: number; rows: number; dirty: boolean; model: string;
  reviews: Review[]; history: string[]; input: string; offset: number; busy: boolean; outcome?: Outcome;
  pendingKeys: string; paste: boolean; lastKey: number; approved: number; rejected: number; tasks: number;
  inputTooLong: boolean;
  calls: number; inputTokens: number; outputTokens: number; cacheRead: number | null; usageComplete: boolean;
};
let active: Session | undefined;

function clean(text: string): string {
  return text.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g,
    c => "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0")).replace(/\t/g, "    ");
}
function width(c: string): number {
  if (/\p{Mark}/u.test(c)) return 0;
  const n = c.codePointAt(0)!;
  return n >= 0x1100 && (n <= 0x115f || n >= 0x2e80 && n <= 0xa4cf || n >= 0xac00 && n <= 0xd7a3 || n >= 0xf900 && n <= 0xfaff || n >= 0xfe10 && n <= 0xfe6f || n >= 0xff00 && n <= 0xff60 || n >= 0xffe0 && n <= 0xffe6 || n >= 0x1f300) ? 2 : 1;
}
function wrap(text: string, columns: number): string[] {
  const result: string[] = [];
  for (const line of clean(text).split("\n")) {
    let part = "", used = 0;
    for (const c of line) {
      const size = width(c);
      if (used + size > columns) { result.push(part); part = ""; used = 0; }
      part += c; used += size;
    }
    result.push(part);
  }
  return result;
}
function oneLine(text: string, columns: number): string { return wrap(text.replace(/\n/g, " "), columns)[0] ?? ""; }
function inputLine(text: string, columns: number): string {
  const chars = [...clean(text)]; let used = 0, start = chars.length;
  while (start > 0 && used + width(chars[start - 1]) <= columns - 2) used += width(chars[--start]);
  return "> " + chars.slice(start).join("");
}
function add(s: Session, text: string) {
  s.history.push(text);
  // Reports are also available in --record; approval requests are never cut.
  if (s.history.length > 200) s.history.splice(0, s.history.length - 200);
  s.offset = Number.MAX_SAFE_INTEGER; s.dirty = true;
}
function reviewText(review: Review): string {
  const r = review.request;
  const assessments = ("assessments" in r ? r.assessments : []).map((value: any, index) => JSON.stringify({
    check: index + 1, decision: value?.decision, reason: value?.reason,
    reason_codes: value?.reason_codes, risk_probabilities: value?.risk_probabilities,
  }, null, 2));
  return [`请求: ${r.id}`, `工具: ${r.tool}`, `目录: ${r.cwd}`, `Fiber: ${r.source.fiber}`,
    "", ...("command" in r ? ["原始命令（仅本次授权，不改写命令）：", r.command]
      : ["原始调用参数（读取或写入前审批，仅本次授权）：", JSON.stringify(r.input, null, 2)]),
    ...(assessments.length ? ["", "风险评估：", ...assessments] : [])].join("\n");
}
function paint(s: Session) {
  if (s.columns < 40 || s.rows < 10) {
    terminal.render(s.id, [oneLine("终端过小：至少需要 40×10；Ctrl-C 退出", Math.max(1, s.columns - 1))]);
    s.dirty = false; return;
  }
  const columns = s.columns - 1, height = s.rows - 6;
  const r = s.reviews[0];
  const content = wrap(r ? reviewText(r) : s.history.join("\n\n"), columns);
  const max = Math.max(0, content.length - height);
  let offset = Math.min(r ? r.offset : s.offset, max);
  if (r) { r.offset = offset; if (offset + height >= content.length) r.seenEnd = true; }
  else s.offset = offset;
  const title = r ? `Wendy · 权限审批 · ${s.reviews.length} 个请求` : "Wendy · TUI 任务台 · " + s.model;
  const status = r ? `仅批准当前请求 · ${offset + 1}–${Math.min(content.length, offset + height)} / ${content.length} 行`
    : `${s.busy ? "执行中" : "就绪"} · agent 对话 · 工具调用需确认 · 完成 ${s.tasks} 个任务`;
  const lines = [oneLine(title, columns), oneLine(status, columns), "─".repeat(columns),
    ...content.slice(offset, offset + height)];
  while (lines.length < s.rows - 3) lines.push("");
  lines.push("─".repeat(columns));
  lines.push(oneLine(r ? r.seenEnd ? "[y] 批准本次  [n/Enter/Esc] 拒绝  [↑↓/Space] 滚动" : "先滚动查看完整请求，再按 y 批准；n 拒绝；Space 下一页"
    : s.busy ? "任务运行中；Ctrl-C 退出并取消任务" : inputLine(s.input, columns), columns));
  lines.push(oneLine(r ? "Ctrl-C 退出并取消任务；模型输出不能代替用户批准" : "/help 帮助  /clear 清屏  /quit 退出  ↑↓ 翻阅", columns));
  const size = terminal.render(s.id, lines);
  s.columns = size.columns; s.rows = size.rows; s.dirty = false;
}
function keys(s: Session, data: string): { key: string; paste?: boolean }[] {
  s.pendingKeys += data;
  if (data) s.lastKey = Date.now();
  const result: { key: string; paste?: boolean }[] = [];
  const names: Record<string, string> = { "\x1b[A": "up", "\x1b[B": "down", "\x1b[5~": "pageup", "\x1b[6~": "pagedown", "\x1b[H": "home", "\x1b[F": "end" };
  while (s.pendingKeys) {
    if (s.paste) {
      const end = s.pendingKeys.indexOf("\x1b[201~");
      if (end < 0) { if (s.pendingKeys.length > 65536) { s.inputTooLong = true; s.pendingKeys = s.pendingKeys.slice(-6); } break; }
      result.push({ key: s.pendingKeys.slice(0, end), paste: true });
      s.pendingKeys = s.pendingKeys.slice(end + 6); s.paste = false; continue;
    }
    if (s.pendingKeys.startsWith("\x1b[200~")) { s.paste = true; s.pendingKeys = s.pendingKeys.slice(6); continue; }
    if (s.pendingKeys.startsWith("\x1b")) {
      const sequence = /^\x1b\[[0-?]*[ -/]*[@-~]/.exec(s.pendingKeys)?.[0];
      if (sequence) { result.push({ key: names[sequence] ?? "ignored" }); s.pendingKeys = s.pendingKeys.slice(sequence.length); continue; }
      if (Date.now() - s.lastKey < 150) break;
      result.push({ key: "escape" }); s.pendingKeys = s.pendingKeys.slice(1); continue;
    }
    const c = String.fromCodePoint(s.pendingKeys.codePointAt(0)!);
    s.pendingKeys = s.pendingKeys.slice(c.length);
    result.push({ key: c === "\r" || c === "\n" ? "enter" : c === "\x03" ? "interrupt" : c === "\x04" ? "eof" : c === "\x7f" || c === "\b" ? "backspace" : c });
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
  s.busy = true; s.outcome = undefined; add(s, "> " + text);
  const operation = plugins.agent({ prompt: text });
  // The TUI fiber owns this await, including each agent/approval descendant.
  Promise.resolve(operation).then(output => { s.outcome = { output }; s.dirty = true; }, error => { s.outcome = { error: String(error) }; s.dirty = true; });
}
function finish(s: Session) {
  const outcome = s.outcome!; s.outcome = undefined; s.busy = false;
  terminal.flush(s.id); s.pendingKeys = ""; s.paste = false;
  if (outcome.error) { s.usageComplete = false; add(s, "任务失败：" + outcome.error + "\n本任务用量请查 --record；会话小计只包含已返回的 agent 用量。"); return; }
  const output = outcome.output;
  if (output?.status === "approval_required") add(s, "任务待审批，尚未完成：\n" + JSON.stringify(output.approval_requests ?? output.approval, null, 2));
  else { s.tasks++; add(s, output?.text ?? output?.content ?? (output?.stdout !== undefined ? output.stdout + output.stderr + `\n退出码：${output.exit_code}` : JSON.stringify(output, null, 2))); }
  usage(s, output?.total_usage);
}

/** Interactive terminal task console; use `wendy tui`. Not registered as a model tool. */
export async function onHandle(input: { prompt?: string }): Promise<{ tasks: number; approved: number; rejected: number; usage: unknown }> {
  if (active) throw new Error("a TUI session is already active");
  if (input?.prompt !== undefined && (typeof input.prompt !== "string" || !input.prompt.trim())) throw new Error("prompt must be a non-empty string");
  const current = await fiber.info();
  const profile = typeof ctx.currentModel === "function" ? await ctx.currentModel() : null;
  const opened = terminal.open();
  const s: Session = { ...opened, fiber: current.id, model: profile?.model ?? "未选择模型", dirty: true, reviews: [], history: [], input: "", offset: 0, busy: false,
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
    add(s, "输入消息并回车，与 agent 交互。工具和委派权限由 agent 管理；读取、写入、执行命令和模型委派均由审批策略检查。\n审批按 y 批准本次，n/Enter/Esc 拒绝。\nCtrl-C 退出，并取消当前任务。");
    if (input?.prompt) launch(s, input.prompt);
    for (;;) {
      s.reviews = s.reviews.filter(r => !r.cancelled);
      if (s.outcome) finish(s);
      if (s.dirty) paint(s);
      const event = await terminal.read(s.id, 100);
      if (event.columns !== s.columns || event.rows !== s.rows) { s.columns = event.columns; s.rows = event.rows; s.dirty = true; }
      let quit = false;
      for (const { key, paste } of keys(s, event.data)) {
        if (key === "interrupt" || key === "eof") { if (s.busy) throw new Error("TUI interrupted by user; active task cancelled"); quit = true; break; }
        if (s.columns < 40 || s.rows < 10) continue;
        const r = s.reviews[0];
        if (r) {
          if (paste) continue;
          const step = Math.max(1, s.rows - 6);
          if (["down", "j"].includes(key)) r.offset++;
          if (["up", "k"].includes(key)) r.offset = Math.max(0, r.offset - 1);
          if (["pagedown", " "].includes(key)) r.offset += step;
          if (key === "pageup") r.offset = Math.max(0, r.offset - step);
          if (key === "y" && r.seenEnd || ["n", "enter", "escape"].includes(key)) {
            const approved = key === "y";
            if (approved) s.approved++; else s.rejected++;
            s.reviews.shift();
            r.resolve({ request_id: r.request.id, decision: approved ? "approved" : "rejected", approver: "local-terminal", reference: "tty:" + r.request.id, reason: approved ? "user approved this operation in TUI" : "user rejected this operation in TUI" });
            add(s, `${approved ? "已批准" : "已拒绝"}本次 ${r.request.tool}：${"command" in r.request ? r.request.command : JSON.stringify(r.request.input)}`);
            terminal.flush(s.id); s.pendingKeys = "";
            // Do not apply leftover keystrokes to a different request.
            s.dirty = true; break;
          }
          s.dirty = true; continue;
        }
        if (["up", "down", "pageup", "pagedown", "home", "end"].includes(key)) {
          s.offset = key === "home" ? 0 : key === "end" ? Number.MAX_SAFE_INTEGER : Math.max(0, s.offset + (key === "up" || key === "pageup" ? -1 : 1) * (key.startsWith("page") ? s.rows - 6 : 1));
          s.dirty = true; continue;
        }
        if (s.busy) continue;
        if (key === "\x15") { s.input = ""; s.inputTooLong = false; s.dirty = true; continue; }
        if (paste) s.input += clean(key).replace(/\n/g, " ");
        else if (key === "backspace") s.input = [...s.input].slice(0, -1).join("");
        else if (key === "enter") {
          const text = s.input.trim(); s.input = "";
          if (s.inputTooLong) { s.inputTooLong = false; add(s, "输入超过 65536 字符，未提交；请重新输入较短任务或脚本。"); continue; }
          if (text === "/quit" || text === "/exit") { quit = true; break; }
          if (text === "/clear") { s.history = []; s.offset = 0; }
          else if (text === "/help") add(s, "输入消息交给 agent；工具目录、权限和委派限制由 agent 管理。/clear 清屏；/quit 退出。审批 y 批准本次、n/Enter/Esc 拒绝、上下/Space 滚动。只有当前终端的明确输入才能批准。");
          else if (text) launch(s, text);
        } else if (key !== "ignored" && key !== "escape" && !/[\x00-\x1f]/.test(key)) s.input += key;
        if (s.input.length > 65536) { s.inputTooLong = true; s.input = s.input.slice(0, 65536); }
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
