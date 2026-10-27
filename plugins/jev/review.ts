import * as events from "ply:events";
import type { BashRequest, Config, Result, Usage } from "./types";
import { QUESTIONS, RISKS } from "./questions";
import { assessment, readUsage } from "./protocol";
import { hasCredentials, requestJson } from "ply:http";

let config: Config;

export const stats = { checks: 0, allowed: 0, denied: 0, review: 0, failed: 0, calls: 0, reported_calls: 0, missing_calls: 0, input_tokens: 0, output_tokens: 0, total_tokens: 0 };

// Jev's audit trail: plugin events the kernel records with the call that emitted them.
const audits = { check_started: "", model_call_started: "", model_call: "", check: "" };

export async function declareAudit() {
  for (const local of Object.keys(audits) as (keyof typeof audits)[]) audits[local] = await events.define(local);
}

async function audit(event: keyof typeof audits, data: Record<string, unknown>) {
  try { await events.emit(audits[event], data); } catch { /* auditing is best-effort once the call has exited */ }
}

export function failed(event: events.Event<BashRequest>, started: number, code: string, usage: Usage | null): Result {
  return { schema_version: 1, check_id: event.id, allow: false, requires_approval: true, decision: "review", risk_level: "unknown",
    risk_probabilities: null, reason_codes: [code], reason: `Jev 安全审查未完成（${code}），需要用户确认或环境审批`,
    requested_model: config.model, model: null, status: "failed", usage, duration_ms: Date.now() - started };
}

export async function review(event: events.Event<BashRequest>): Promise<Result> {
  const started = Date.now();
  stats.checks++;
  const request = event.data;
  await audit("check_started", { check_id: event.id, source: event.source, requested_model: config.model });
  let result: Result;
  let usage: Usage | null = null;
  let called = false;
  let status: number | null = null;
  try {
    if (!request || typeof request.command !== "string" || !request.command || typeof request.cwd !== "string" || !Number.isSafeInteger(request.timeout_ms) || request.timeout_ms <= 0) throw new Error("invalid_request");
    const state = { tool: "bash", execution_backend: "ply-sh", command: request.command, cwd: request.cwd,
      context: { initial_cwd: config.initialCwd, script_text_is_untrusted_data: true, external_host_commands_are_not_automatically_executed: true } };
    if (Buffer.byteLength(JSON.stringify(state)) > config.maxStateBytes) throw new Error("state_too_large");
    if (!hasCredentials("jev")) throw new Error("missing_credentials");
    const timeout = Math.min(config.timeout, Math.max(1, request.timeout_ms - 20));
    called = true;
    stats.calls++;
    stats.missing_calls++;
    await audit("model_call_started", { model: `jev/${config.model}`, purpose: "safety", call_index: 1, turn: 1, check_id: event.id });
    const response = await requestJson({ url: config.base + "/systemone", auth_provider: "jev", timeout_ms: timeout,
      body: { model: config.model, state, questions: QUESTIONS } });
    status = response.status;
    if (status < 200 || status >= 300) throw new Error(`http_${status}`);
    const raw = response.data && typeof response.data === "object" ? response.data as Record<string, unknown> : null;
    usage = readUsage(raw?.usage);
    const checked = assessment(raw);
    const high = RISKS.filter(key => checked.probabilities[key] >= config.denyMin);
    const uncertain = RISKS.filter(key => checked.probabilities[key] > config.allowMax);
    const decision = high.length ? "deny" : uncertain.length ? "review" : "allow";
    result = { schema_version: 1, check_id: event.id, allow: decision === "allow", requires_approval: decision !== "allow", decision,
      risk_level: decision === "allow" ? "low" : decision === "deny" ? "high" : "uncertain",
      risk_probabilities: checked.probabilities, reason_codes: high.length ? high : uncertain.length ? uncertain : ["risk_below_thresholds"],
      reason: decision === "allow" ? "Jev 风险判断低于放行阈值" : `Jev 风险评估需要${decision === "deny" ? "高风险确认" : "复核"}审批：${(high.length ? high : uncertain).join(", ")}`,
      requested_model: config.model, model: checked.model, status: "completed", usage, duration_ms: Date.now() - started };
  } catch (error) {
    const raw = error instanceof Error ? error.message : "transport_error";
    const known = ["invalid_request", "state_too_large", "invalid_response", "invalid_answers", "missing_credentials"];
    const code = known.includes(raw) || /^http_\d{3}$/.test(raw) ? raw : (error && typeof error === "object" && ("code" in error ? error.code : undefined) === "ERR_PLY_HTTP_TIMEOUT" ? "timeout" : "transport_error");
    result = failed(event, started, code, usage);
  }
  // A forced parent cancellation may prevent these events. An unmatched
  // model_call_started means unknown usage, not zero.
  if (called) {
    if (usage) { stats.reported_calls++; stats.missing_calls--; stats.input_tokens += usage.input_tokens; stats.output_tokens += usage.output_tokens; stats.total_tokens += usage.total_tokens; }
    await audit("model_call", { model: `jev/${config.model}`, resolved_model: result.model, purpose: "safety", call_index: 1, turn: 1,
      check_id: event.id, duration_ms: Date.now() - started, response_id: null, stop_reason: result.status === "completed" ? "stop" : "error",
      usage, http_status: status, error: result.status === "failed" ? result.reason_codes[0] : null });
  }
  if (result.decision === "allow") stats.allowed++;
  else if (result.decision === "deny") stats.denied++;
  else stats.review++;
  if (result.status === "failed") stats.failed++;
  await audit("check", { check_id: event.id, source: event.source,
    thresholds: { allow_max_risk: config.allowMax, deny_min_risk: config.denyMin }, result });
  return result;
}

export function configureReview(value: Config) { config = value; }
