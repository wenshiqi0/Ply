import * as commands from "ply:commands";
import * as events from "ply:events";
import * as terminal from "ply:terminal";
import plugins from "ply:plugins";
import type { Decision, Review, Session, TuiInput, TuiResult } from "./types";
import { add, paint } from "./render";
import { context } from "ply:plugins";
import { edit } from "./composer";
import { finish, launch } from "./session";
import { keys } from "./keys";

let active: Session | undefined;

/** Interactive terminal task console; use `ply tui`. Not registered as a model tool. */
export async function onHandle(input: TuiInput): Promise<TuiResult> {
  if (active) throw new Error("a TUI session is already active");
  if (input?.prompt !== undefined && (typeof input.prompt !== "string" || !input.prompt.trim())) throw new Error("prompt must be a non-empty string");
  const profile = typeof context.currentModel === "function" ? await context.currentModel() : null;
  const approvalStatus = plugins.approval.approvalStatus;
  const approval = typeof approvalStatus === "function" ? await approvalStatus() : null;
  const opened = terminal.open();
  const s: Session = { ...opened, model: profile?.model ?? "未选择模型", dirty: true, reviews: [], history: [], input: "", caret: 0, inputView: 0, offset: 0, busy: false,
    approvalMode: approval?.mode ?? "risk",
    sent: [], recalled: 0, draft: "", notice: "", screenKey: "", startedAt: 0, lastPulse: 0, bodyHeight: 1,
    pendingKeys: "", paste: false, lastKey: 0, approved: 0, rejected: 0, tasks: 0, inputTooLong: false,
    calls: 0, inputTokens: 0, outputTokens: 0, cacheRead: 0, usageComplete: true };
  active = s;
  const close = () => {
    active = undefined;
    for (const r of s.reviews.splice(0)) { r.cancelled = true; r.resolve({ request_id: r.request.id, decision: "pending", reason: "terminal closed" }); }
    try { terminal.close(s.id); } catch { /* Terminal may already be closed by cancellation. */ }
  };
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
      for (const { key, paste, text } of keys(s, event.data)) {
        // Typed text is never a key name, whatever its letters spell.
        if (text) { if (s.columns >= 40 && s.rows >= 10) { edit(s, key, true); s.dirty = true; } continue; }
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
    return result;
  } finally { close(); }
}

/** Environment adapter only; never adds an approval or TUI tool to the model. */
export async function onLoad() {
  // One terminal session per process answers every approval while it runs.
  const review = async (event: events.Event<PlyApprovalRequest | PlyToolApprovalRequest>): Promise<Decision> => {
    const s = active;
    if (!s) return { request_id: event.data.id, decision: "pending", reason: "no active TUI session" };
    let review!: Review;
    const answer = new Promise<Decision>(resolve => { review = { request: event.data, resolve, cancelled: false, offset: 0, seenEnd: false }; });
    try {
      // A fresh review must never consume keys buffered for task input.
      terminal.flush(s.id); s.pendingKeys = ""; s.paste = false;
      s.reviews.push(review); s.dirty = true;
      return await answer;
    } finally { review.cancelled = true; s.dirty = true; }
  };
  await events.on("approval.request", review);
  await events.on("approval.toolRequest", review);
  await commands.register({ name: "tui", description: "与 agent 交互并处理工具审批", handler: onHandle });
}
