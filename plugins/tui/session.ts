import plugins from "ply:plugins";
import type { Session, TaskUsage } from "./types";
import { add } from "./render";

export function usage(s: Session, value: TaskUsage | undefined) {
  if (!value) return;
  s.calls += value.calls ?? 0; s.inputTokens += value.input_tokens ?? 0; s.outputTokens += value.output_tokens ?? 0;
  s.cacheRead = s.cacheRead === null || value.cache_read_tokens === null || value.cache_read_tokens === undefined ? null : s.cacheRead + value.cache_read_tokens;
  s.usageComplete = s.usageComplete && value.complete === true;
  const rate = value.cache_hit_rate == null ? "未知" : (value.cache_hit_rate * 100).toFixed(1) + "%";
  add(s, `本任务用量：调用 ${value.calls} · 输入 ${value.input_tokens} · 输出 ${value.output_tokens} · 缓存命中 ${rate}${value.complete ? "" : " · 用量不完整"}`);
}

export function launch(s: Session, text: string) {
  s.busy = true; s.outcome = undefined; s.startedAt = Date.now(); s.lastPulse = 0; s.notice = ""; add(s, text, "user");
  const operation = plugins.agent({ prompt: text });
  // The agent runs under the TUI call: cancelling the TUI cancels it too.
  Promise.resolve(operation).then(output => { s.outcome = { output }; s.dirty = true; }, error => { s.outcome = { error: String(error) }; s.dirty = true; });
}

export function finish(s: Session) {
  const outcome = s.outcome!; s.outcome = undefined; s.busy = false;
  s.notice = "";
  if (outcome.error) { s.usageComplete = false; add(s, "任务失败：" + outcome.error + "\n本任务用量请查 --record；会话小计只包含已返回的 agent 用量。", "error"); return; }
  const output = outcome.output;
  if (output?.status === "approval_required") add(s, "任务待审批，尚未完成：\n" + JSON.stringify(output.approval_requests ?? output.approval, null, 2));
  else { s.tasks++; add(s, output?.text ?? output?.content ?? (output?.stdout !== undefined ? output.stdout + output.stderr + `\n退出码：${output.exit_code}` : JSON.stringify(output, null, 2)), "assistant"); }
  usage(s, output?.total_usage);
}
