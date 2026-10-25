import plugins from "wendy:plugins";
import { request as modelRequest } from "wendy:models";
import type { ModelProfile } from "wendy:models";
import * as checkpoints from "wendy:checkpoints";
import { onDispose } from "wendy:lifecycle";

// The loop and pi-ai model calls run in this plugin's JSC realm. The host
// supplies HTTP/TLS and streams; no external model process is started.
type Block = { type: string; text?: string; id?: string; name?: string; arguments?: Record<string, unknown> };
type Message = {
  role: string;
  content: unknown;
  stopReason?: string;
  errorMessage?: string;
  responseId?: string;
  usage?: unknown;
  approval_requests?: unknown[];
  timestamp: number;
};
type Context = { systemPrompt: string; messages: Message[]; tools: { name: string }[] };
type Budget = { reserve_output_tokens: number; estimated_input_tokens: number; input_budget_tokens: number; [key: string]: unknown };
type PreparedContext = {
  revision: number;
  tools_signature: string;
  budget: Budget;
  context?: Context;
  compaction?: { context: Context; max_output_tokens: number; estimated_input_tokens: number; messages_to_summarize: number };
};

type TokenUsage = {
  input_tokens: number;
  uncached_input_tokens: number | null;
  output_tokens: number;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  reasoning_tokens: number | null;
  total_tokens: number;
  cache_hit_rate: number | null;
};

type UsageCounts = {
  calls: number;
  reported_calls: number;
  missing_calls: number;
  input_tokens: number;
  uncached_input_tokens: number | null;
  output_tokens: number;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  reasoning_tokens: number | null;
  total_tokens: number;
};

type UsageSummary = {
  calls: number;
  reported_calls: number;
  missing_calls: number;
  complete: boolean;
  input_tokens: number;
  uncached_input_tokens: number | null;
  output_tokens: number;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  reasoning_tokens: number | null;
  total_tokens: number;
  cache_hit_rate: number | null;
};

type RunUsage = { own: UsageCounts; total: UsageCounts; parent?: RunUsage; policy: AgentPolicy; report: ReportState; compaction_retry_after: number; external: Map<string, { finished: boolean; usage: TokenUsage | null }> };
// Calls of this plugin share a realm. Link active agents by the kernel's
// ancestry, so even a child that fails contributes usage to its ancestors.
const activeUsage = new Map<string, RunUsage>();

type AgentPolicy = { allowed_tools: string[]; max_delegation_depth: number; max_children: number };
type AgentInput = {
  /** A self-contained task. Required for a new agent; omit when resuming. */
  prompt?: string;
  /** Resume a private recovery record returned by a failed agent. */
  resume_id?: string;
  /** Model-visible tool whitelist; children can only narrow their parent's list. */
  allowed_tools?: string[];
  /** Child generations this agent may create. Root default 1; children inherit remaining depth minus 1. Zero forbids delegation. */
  max_delegation_depth?: number;
  /** Cumulative direct-child agent limit, including failures/replacements/resumes (root default 2). Children may only narrow it. */
  max_children?: number;
  /** Required Markdown report section headings, inherited by children and retained on resume. */
  report_sections?: string[];
  /** Optional positive limit on task-model turns in this invocation. */
  max_turns?: number;
};
type Recovery = {
  format: 2;
  phase: "ready" | "tools_pending";
  cwd: string;
  profile: ModelProfile;
  policy: AgentPolicy;
  children_started: number;
  report: ReportState;
  conversation: unknown;
  next_turn: number;
  tool_calls: number;
  previous_usage: UsageSummary;
  previous_total_usage: UsageSummary;
  pending_tools?: Block[];
};

function integer(value: unknown, name: string, minimum = 0): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < minimum) throw new Error(name + " must be a safe integer >= " + minimum);
  return value;
}
const REPORT_END = "[[WENDY_REPORT_COMPLETE]]";
type ReportState = { required_sections: string[]; rejected_reports: number };

function sectionNames(value: unknown, name: string): string[] {
  if (!Array.isArray(value) || value.length > 16 || value.some(section => typeof section !== "string" || !section.trim() || section.length > 128 || /[\r\n]/.test(section))) {
    throw new Error(name + " must contain at most 16 non-empty, single-line section headings");
  }
  return [...new Set(value.map(section => (section as string).trim()))];
}
function reportFor(input: AgentInput, parent?: RunUsage, saved?: ReportState): ReportState {
  const defaults = sectionNames(JSON.parse(process.env.WENDY_AGENT_REPORT_SECTIONS ?? "[]"), "default report sections");
  const requested = input.report_sections === undefined ? (parent || saved ? [] : defaults) : sectionNames(input.report_sections, "report_sections");
  return { required_sections: sectionNames([...new Set([...(parent?.report.required_sections ?? []), ...(saved?.required_sections ?? []), ...requested])], "combined report sections"),
    rejected_reports: saved?.rejected_reports ?? 0 };
}
function headingName(value: string): string {
  return value.trim().replace(/^\d+[.)、\s]+/, "").replace(/^[一二三四五六七八九十]+[、.)]\s*/, "").trim().toLowerCase();
}
function validateReport(text: string, stop: string | undefined, report: ReportState): { text: string; errors: string[] } {
  const errors: string[] = [];
  const trimmed = text.trim();
  const lines = trimmed.split(/\r?\n/);
  if (lines.at(-1)?.trim() !== REPORT_END) errors.push("missing_completion_marker");
  else lines.pop();
  const body = lines.join("\n").trim();
  if (!body) errors.push("empty_report");
  if (stop !== "stop") errors.push(stop === "length" ? "truncated_report" : "unexpected_stop_reason");
  const headings: { name: string; line: number; depth: number }[] = [];
  const meaningful: boolean[] = [];
  let fence: { char: string; length: number } | undefined;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    meaningful[i] = false;
    const marker = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line);
    if (fence) {
      if (marker && marker[1][0] === fence.char && marker[1].length >= fence.length && !marker[2].trim()) fence = undefined;
      else meaningful[i] = !!line.trim();
      continue;
    }
    if (marker) { fence = { char: marker[1][0], length: marker[1].length }; continue; }
    const visible = line.replace(/(`+)[^`]*\1/g, "");
    if (visible.includes(REPORT_END)) errors.push("unexpected_completion_marker");
    if (/<\/?[｜|]{2}DSML[｜|]{2}[^>\n]*>/iu.test(visible) || /<[｜|]tool[_▁][^>\n]*[｜|]>/iu.test(visible)) errors.push("residual_protocol_tokens");
    const heading = /^ {0,3}(#{1,6})\s+(.+?)\s*#*\s*$/.exec(line);
    if (heading) headings.push({ name: headingName(heading[2]), line: i, depth: heading[1].length });
    else meaningful[i] = !!line.trim();
  }
  if (fence) errors.push("unclosed_code_fence");
  for (const required of report.required_sections) {
    const candidates = headings.filter(heading => heading.name === headingName(required));
    if (!candidates.length) { errors.push("missing_section:" + required); continue; }
    const nonempty = candidates.some(heading => {
      const end = headings.find(next => next.line > heading.line && next.depth <= heading.depth)?.line ?? lines.length;
      return meaningful.slice(heading.line + 1, end).some(Boolean);
    });
    if (!nonempty) errors.push("empty_section:" + required);
  }
  return { text: body, errors: [...new Set(errors)] };
}

function recovery(value: any): Recovery {
  if (!value || value.format !== 2) throw new Error("unsupported recovery format: expected 2; older checkpoints cannot be resumed; start a new task");
  if ( !["ready", "tools_pending"].includes(value.phase) || value.cwd !== process.cwd() ||
      !value.profile || typeof value.profile.provider !== "string" || typeof value.profile.model !== "string" ||
      !value.policy || !Array.isArray(value.policy.allowed_tools)) throw new Error("invalid recovery record or workspace mismatch");
  integer(value.next_turn, "checkpoint next_turn", 1);
  integer(value.tool_calls, "checkpoint tool_calls");
  integer(value.policy.max_delegation_depth, "checkpoint delegation depth");
  integer(value.policy.max_children, "checkpoint child limit");
  integer(value.children_started, "checkpoint children_started");
  if (!value.report || typeof value.report !== "object") throw new Error("invalid checkpoint report state");
  value.report.required_sections = sectionNames(value.report.required_sections, "checkpoint report sections");
  integer(value.report.rejected_reports, "checkpoint rejected_reports");
  if (value.policy.allowed_tools.some((name: unknown) => typeof name !== "string" || !/^[a-zA-Z0-9_-]{1,64}$/.test(name))) {
    throw new Error("invalid checkpoint tool permissions");
  }
  return value as Recovery;
}
function policyFor(input: AgentInput, available: string[], parent?: RunUsage, saved?: AgentPolicy): AgentPolicy {
  if (parent && parent.policy.max_delegation_depth === 0) throw new Error("the parent agent is not allowed to delegate");
  const ceiling = Math.min(parent ? parent.policy.max_delegation_depth - 1 : (input.max_delegation_depth ?? saved?.max_delegation_depth ?? 1),
    saved?.max_delegation_depth ?? Number.MAX_SAFE_INTEGER);
  const depth = input.max_delegation_depth === undefined ? ceiling : integer(input.max_delegation_depth, "max_delegation_depth");
  if (depth > ceiling) throw new Error("a child/resumed agent cannot increase delegation depth");
  const base = available.filter(name => (!parent || parent.policy.allowed_tools.includes(name)) && (!saved || saved.allowed_tools.includes(name)));
  let names = base;
  if (input.allowed_tools !== undefined) {
    if (!Array.isArray(input.allowed_tools) || input.allowed_tools.some(name => typeof name !== "string" || !base.includes(name))) {
      throw new Error("allowed_tools contains an unavailable or unauthorized tool");
    }
    names = [...new Set(input.allowed_tools)];
  }
  const defaultChildren = integer(Number(process.env.WENDY_AGENT_MAX_CHILDREN ?? 2), "default max_children");
  const childCeiling = Math.min(parent ? parent.policy.max_children : (input.max_children ?? saved?.max_children ?? defaultChildren),
    saved?.max_children ?? Number.MAX_SAFE_INTEGER);
  const children = input.max_children === undefined ? childCeiling : integer(input.max_children, "max_children");
  if (children > childCeiling) throw new Error("a child/resumed agent cannot increase max_children");
  names = names.filter(name => name !== "agent" || depth > 0 && children > 0).sort();
  return { allowed_tools: names, max_delegation_depth: names.includes("agent") ? depth : 0,
    max_children: names.includes("agent") ? children : 0 };
}

/** Run a task with bounded tool/delegation grants, or resume a recovery ID. */
export async function onHandle(input: AgentInput): Promise<{
  text: string; model: string; turns: number; total_turns: number; tool_calls: number;
  status?: "approval_required"; approval_requests?: unknown[];
  usage: UsageSummary; total_usage: UsageSummary; policy: AgentPolicy; resumed_from?: string;
  delegation: { limit: number; started: number; remaining: number };
  report_validation: ReportState;
  previous_usage?: UsageSummary; previous_total_usage?: UsageSummary;
}> {
  if (!input || typeof input !== "object") throw new Error("agent input must be an object");
  if (input.resume_id !== undefined && (typeof input.resume_id !== "string" || !input.resume_id.trim() || input.prompt !== undefined)) {
    throw new Error("resume_id must be a string; omit prompt when resuming");
  }
  if (input.resume_id === undefined && (typeof input.prompt !== "string" || !input.prompt.trim())) throw new Error("'prompt' must be a non-empty string");
  const maxTurns = input.max_turns === undefined ? undefined : integer(input.max_turns, "max_turns", 1);
  if (input.max_children !== undefined) integer(input.max_children, "max_children");
  if (input.report_sections !== undefined) sectionNames(input.report_sections, "report_sections");
  if (input.max_delegation_depth !== undefined) integer(input.max_delegation_depth, "max_delegation_depth");
  await plugins.context.loaded;
  const current = await fiber.info();
  let run: RunUsage | undefined;
  let id: string | undefined;
  let lease: number | undefined;
  let checkpointId = input.resume_id;
  let profile: ModelProfile | undefined;
  let policy: AgentPolicy | undefined;
  let report: ReportState = { required_sections: [], rejected_reports: 0 };
  let model = "";
  let completed = false;
  let approvalRequired = false;
  let safe = true;
  let nextTurn = 1;
  let toolCalls = 0;
  let lastReady: Recovery | undefined;
  let previousUsage: UsageSummary | undefined;
  let previousTotalUsage: UsageSummary | undefined;
  const unwatch = onDispose(async () => {
    // A forced VM interruption cannot execute this generator's finally.
    activeUsage.delete(current.id);
    try { if (id !== undefined) await ctx.closeConversation(id); }
    finally { if (lease !== undefined) await checkpoints.finish(lease, false); }
  });
  const snapshot = async (): Promise<Recovery> => ({
    format: 2, phase: "ready", cwd: process.cwd(), profile: profile!, policy: policy!,
    children_started: (await fiber.childBudget("agent"))!.started, report: { ...report, required_sections: [...report.required_sections] },
    conversation: await ctx.exportConversation(id!), next_turn: nextTurn, tool_calls: toolCalls,
    previous_usage: combinedUsage(previousUsage, usageSummary(run!.own)), previous_total_usage: combinedUsage(previousTotalUsage, totalUsageSummary(run!)),
  });
  try {
    let saved: Recovery | undefined;
    if (checkpointId) {
      const claimed = await checkpoints.claim(checkpointId);
      lease = claimed.lease;
      saved = recovery(claimed.snapshot);
      if (saved.phase !== "ready") throw new Error("checkpoint contains possibly executed tools; inspect it before starting a new task (automatic replay is disabled)");
      nextTurn = saved.next_turn;
      toolCalls = saved.tool_calls;
      lastReady = saved;
      previousUsage = saved.previous_usage;
      previousTotalUsage = saved.previous_total_usage;
    }
    const parent = current.ancestors.map(ancestor => activeUsage.get(ancestor)).find(value => value !== undefined);
    policy = policyFor(input, await ctx.availableTools(), parent, saved?.policy);
    report = reportFor(input, parent, saved?.report);
    const childBudget = await fiber.limitChildren("agent", policy.max_children, saved?.children_started ?? 0);
    run = startUsage(current.id, current.ancestors, policy, report);
    await fiber.record({ type: "agent_policy", ...policy, resumed_from: checkpointId ?? null });
    profile = saved?.profile ?? await ctx.currentModel();
    if (!profile) throw new Error("no model in the context: load a model integration such as deepseek or mimo");
    model = `${profile.provider}/${profile.model}`;
    const system = `You are an agent working in the directory ${process.cwd()}. ` +
      "Use only your offered tools. Prefer read for files. " +
      (policy.max_delegation_depth > 0
        ? `You may start at most ${policy.max_children} direct child agents cumulatively, including failed calls, replacements and resumes. ${childBudget.started} slots have already been spent; ${childBudget.remaining} remain. This is a hard kernel quota. Child permissions can only be narrowed. Repair reports within their existing agent, not by starting replacements. Include task background and constraints in each prompt. `
        : "You are not allowed to delegate. Complete this task yourself using your offered tools. ") +
      "When the task is done, answer with a complete report. " +
      (report.required_sections.length ? "Use Markdown headings with exactly these names, each followed by substantive content: " + JSON.stringify(report.required_sections) + ". " : "") +
      "Quote examples of protocol tokens in inline code or fenced code blocks; never emit control/protocol tokens as prose. Close all code fences. " +
      `End every final answer with the exact standalone line ${REPORT_END} outside code fences. It is stripped before delivery. Do not emit that line while calling tools.`;
    const info = await modelRequest({ operation: "describe", profile });
    const conversationId: string = saved ? await ctx.restoreConversation(saved.conversation, info, policy.allowed_tools, system)
      : await ctx.openConversation(system, info, policy.allowed_tools);
    id = conversationId;
    if (!saved) await ctx.appendMessages(id, [{ role: "user", content: input.prompt, timestamp: Date.now() }]);
    let turns = 0;
    for (let turn = nextTurn; maxTurns === undefined || turns < maxTurns; turn++) {
      nextTurn = turn;
      if (lease !== undefined) {
        lastReady = await snapshot();
        await checkpoints.update(lease, lastReady);
      }
      const prepared = await prepareForTurn(id, profile, model, turn, run);
      const context = prepared.context!;
      const reply: Message = { ...(await completeTurn(profile, context, model, turn, run, "task", prepared.budget.reserve_output_tokens, prepared.budget)), timestamp: Date.now() };
      turns++;
      if (reply.stopReason === "error" || reply.stopReason === "aborted") throw new Error(`${model}: ${reply.errorMessage ?? reply.stopReason}`);
      const blocks = (reply.content ?? []) as Block[];
      const calls = blocks.filter(block => block.type === "toolCall");
      // Do not execute any partial tool arguments or add them to history.
      if (reply.stopReason === "length" && calls.length) throw new Error(`${model}: model response truncated during tool calls`);
      if (!calls.length) {
        const candidate = blocks.filter(block => block.type === "text").map(block => block.text ?? "").join("");
        const checked = validateReport(candidate, reply.stopReason, report);
        safe = true;
        if (checked.errors.length) {
          report.rejected_reports++;
          await fiber.record({ type: "agent_report_rejected", model, turn, call_index: run.own.calls,
            response_id: reply.responseId ?? null, reasons: checked.errors, preview: candidate.slice(-512), rejected_reports: report.rejected_reports });
          // Keep malformed protocol text out of the next provider context.
          // Regenerate in this same agent; no extra child or slot is created.
          await ctx.appendMessages(id, [{ role: "user", timestamp: Date.now(), content:
            "Your last final report was rejected: " + checked.errors.join(", ") + ". " +
            "Regenerate the complete answer from the retained evidence in this same agent. Keep it concise enough to fit the output budget. Do not delegate a replacement. " +
            (report.required_sections.length ? "Required Markdown headings: " + JSON.stringify(report.required_sections) + ". " : "") +
            "Close code fences, omit protocol tokens from prose, and finish with the exact standalone line " + REPORT_END + "." }],
            { expected_revision: prepared.revision });
          nextTurn = turn + 1;
          continue;
        }
        const observed = tokenUsage(reply.usage);
        await ctx.appendMessages(id, [reply], { expected_revision: prepared.revision, tools_signature: prepared.tools_signature,
          ...(observed ? { context_tokens: observed.total_tokens } : {}) });
        await fiber.record({ type: "agent_report_validated", model, turn, call_index: run.own.calls,
          required_sections: report.required_sections, rejected_reports: report.rejected_reports });
        const delegation = (await fiber.childBudget("agent"))!;
        completed = true;
        return { text: checked.text, model, turns, total_turns: turn, tool_calls: toolCalls, usage: usageSummary(run.own), total_usage: totalUsageSummary(run), policy,
          delegation, report_validation: { ...report, required_sections: [...report.required_sections] },
          ...(input.resume_id ? { resumed_from: input.resume_id, previous_usage: previousUsage, previous_total_usage: previousTotalUsage } : {}) };
      }
      const observed = tokenUsage(reply.usage);
      await ctx.appendMessages(id, [reply], { expected_revision: prepared.revision, tools_signature: prepared.tools_signature,
        ...(observed ? { context_tokens: observed.total_tokens } : {}) });
      safe = false;
      if (lease !== undefined) await checkpoints.update(lease, { ...lastReady!, phase: "tools_pending", pending_tools: calls });
      toolCalls += calls.length;
      const offered = context.tools.map(tool => tool.name);
      const results = await Promise.all(calls.map(call => runTool(call, offered, policy!)));
      await ctx.appendMessages(id, results);
      const approvals = results.flatMap(result => result.approval_requests ?? []);
      if (approvals.length) {
        approvalRequired = true;
        await fiber.record({ type: "agent_approval_required", approvals });
        // No extra model turns or replacement commands while user action is
        // required. A prior tools_pending lease remains non-replayable.
        return { status: "approval_required", approval_requests: approvals,
          text: "部分工具调用需要用户确认或环境审批；待审批命令尚未执行。", model, turns, total_turns: turn,
          tool_calls: toolCalls, usage: usageSummary(run.own), total_usage: totalUsageSummary(run), policy,
          delegation: (await fiber.childBudget("agent"))!, report_validation: { ...report, required_sections: [...report.required_sections] },
          ...(input.resume_id ? { resumed_from: input.resume_id, previous_usage: previousUsage, previous_total_usage: previousTotalUsage } : {}) };
      }
      safe = true;
      nextTurn = turn + 1;
      if (lease !== undefined) {
        lastReady = await snapshot();
        await checkpoints.update(lease, lastReady);
      }
    }
    throw new Error(`no answer after ${maxTurns} model turns`);
  } catch (error) {
    let message = error instanceof Error ? error.message : String(error);
    if (safe && id && run && profile && policy) {
      try {
        const state = await snapshot();
        if (lease !== undefined) await checkpoints.update(lease, state);
        else checkpointId = await checkpoints.create(state);
        await fiber.record({ type: "agent_checkpoint", resume_id: checkpointId, next_turn: nextTurn, reason: message });
        message += `; resume with agent({ resume_id: "${checkpointId}" })`;
      } catch (saveError) {
        message += "; checkpoint save failed: " + (saveError instanceof Error ? saveError.message : String(saveError));
      }
    }
    throw new Error(message);
  } finally {
    try {
      if (run) await fiber.record({ type: "agent_usage", model, status: approvalRequired ? "approval_required" : completed ? "completed" : "failed", usage: usageSummary(run.own), total_usage: totalUsageSummary(run) });
    } finally {
      activeUsage.delete(current.id);
      try { if (id !== undefined) await ctx.closeConversation(id); }
      finally {
        unwatch();
        if (lease !== undefined) {
          try { await checkpoints.finish(lease, completed); }
          catch (error) {
            await checkpoints.finish(lease, false);
            console.error("checkpoint cleanup failed: " + (error instanceof Error ? error.message : String(error)));
          }
        }
      }
    }
  }
}

function emptyUsage(): UsageCounts {
  return {
    calls: 0, reported_calls: 0, missing_calls: 0,
    input_tokens: 0, uncached_input_tokens: 0, output_tokens: 0,
    cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0, total_tokens: 0,
  };
}

function combinedUsage(previous: UsageSummary | undefined, current: UsageSummary): UsageSummary {
  if (!previous) return current;
  const counts = emptyUsage();
  for (const key of ["calls", "reported_calls", "missing_calls", "input_tokens", "output_tokens", "total_tokens"] as const) {
    counts[key] = integer(integer(previous[key], "checkpoint usage " + key) + current[key], "combined usage " + key);
  }
  for (const key of ["uncached_input_tokens", "cache_read_tokens", "cache_write_tokens"] as const) {
    counts[key] = previous[key] === null || current[key] === null ? null
      : integer(integer(previous[key], "checkpoint usage " + key) + current[key], "combined usage " + key);
  }
  counts.reasoning_tokens = previous.reasoning_tokens === null || current.reasoning_tokens === null ? null
    : integer(previous.reasoning_tokens, "checkpoint reasoning_tokens") + current.reasoning_tokens;
  return usageSummary(counts);
}

function startUsage(id: string, ancestors: string[], policy: AgentPolicy, report: ReportState): RunUsage {
  let parent: RunUsage | undefined;
  for (const ancestor of ancestors) {
    parent = activeUsage.get(ancestor);
    if (parent) break;
  }
  const run: RunUsage = { own: emptyUsage(), total: emptyUsage(), parent, policy, report, compaction_retry_after: 0, external: new Map() };
  activeUsage.set(id, run);
  return run;
}

function isTokenCount(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function tokenUsage(raw: any): TokenUsage | null {
  if (!raw || ![raw.input, raw.output, raw.cacheRead, raw.cacheWrite].every(isTokenCount)) return null;
  // pi-ai's input excludes cache reads/writes; output includes reasoning.
  const input = raw.input + raw.cacheRead + raw.cacheWrite;
  const total = input + raw.output;
  // The SDK also uses all-zero usage as a placeholder before usage arrives.
  if (!isTokenCount(input) || !isTokenCount(total) || total === 0) return null;
  return {
    input_tokens: input,
    uncached_input_tokens: raw.input,
    output_tokens: raw.output,
    cache_read_tokens: raw.cacheRead,
    cache_write_tokens: raw.cacheWrite,
    reasoning_tokens: isTokenCount(raw.reasoning) && raw.reasoning <= raw.output ? raw.reasoning : null,
    total_tokens: total,
    cache_hit_rate: input > 0 ? raw.cacheRead / input : null,
  };
}

function nullableSum(left: number | null, right: number | null): number | null {
  return left === null || right === null ? null : left + right;
}

function addUsage(counts: UsageCounts, usage: TokenUsage | null) {
  counts.calls++;
  if (!usage) {
    counts.missing_calls++;
    counts.reasoning_tokens = null;
    return;
  }
  counts.reported_calls++;
  counts.input_tokens += usage.input_tokens;
  counts.uncached_input_tokens = nullableSum(counts.uncached_input_tokens, usage.uncached_input_tokens);
  counts.output_tokens += usage.output_tokens;
  counts.cache_read_tokens = nullableSum(counts.cache_read_tokens, usage.cache_read_tokens);
  counts.cache_write_tokens = nullableSum(counts.cache_write_tokens, usage.cache_write_tokens);
  counts.total_tokens += usage.total_tokens;
  counts.reasoning_tokens = counts.reasoning_tokens === null || usage.reasoning_tokens === null
    ? null : counts.reasoning_tokens + usage.reasoning_tokens;
}

function rememberUsage(run: RunUsage, usage: TokenUsage | null) {
  addUsage(run.own, usage);
  for (let ancestor: RunUsage | undefined = run; ancestor; ancestor = ancestor.parent) addUsage(ancestor.total, usage);
}

function usageSummary(counts: UsageCounts): UsageSummary {
  const complete = counts.missing_calls === 0;
  return {
    ...counts,
    complete,
    // Weight by input tokens, not by the number of calls. A partial run
    // cannot claim a hit rate for all of its calls.
    cache_hit_rate: complete && counts.input_tokens > 0 && counts.cache_read_tokens !== null ? counts.cache_read_tokens / counts.input_tokens : null,
  };
}

// External callers keep a pending entry from before the request until
// completion. Cancellation leaves unknown usage visible in ancestor totals.
function totalUsageSummary(run: RunUsage): UsageSummary {
  const counts = { ...run.total };
  for (const call of run.external.values()) addUsage(counts, call.usage);
  return usageSummary(counts);
}

function externalUsage(raw: any): TokenUsage | null {
  if (raw === null || raw === undefined) return null;
  const input = integer(raw.input_tokens, "external input_tokens");
  const output = integer(raw.output_tokens, "external output_tokens");
  const total = integer(raw.total_tokens, "external total_tokens");
  if (total !== input + output) throw new Error("invalid external token totals");
  const optional = (key: string): number | null => raw[key] === null || raw[key] === undefined ? null : integer(raw[key], "external " + key);
  const uncached = optional("uncached_input_tokens");
  const read = optional("cache_read_tokens");
  const write = optional("cache_write_tokens");
  const reasoning = optional("reasoning_tokens");
  if (uncached !== null && read !== null && write !== null && uncached + read + write !== input) throw new Error("invalid external input breakdown");
  if (reasoning !== null && reasoning > output) throw new Error("invalid external reasoning_tokens");
  return { input_tokens: input, output_tokens: output, total_tokens: total, uncached_input_tokens: uncached,
    cache_read_tokens: read, cache_write_tokens: write, reasoning_tokens: reasoning, cache_hit_rate: read !== null && input > 0 ? read / input : null };
}

async function reportExternalModelCall(input: { phase: string; call_index: number; usage?: unknown }) {
  if (!input || !["started", "completed"].includes(input.phase)) throw new Error("invalid external model call phase");
  integer(input.call_index, "external call_index", 1);
  const current = await fiber.info();
  const run = current.ancestors.map(id => activeUsage.get(id)).find(run => run !== undefined);
  if (!run) return;
  const key = current.id + ":" + input.call_index;
  const existing = run.external.get(key);
  if (input.phase === "started") {
    if (existing) throw new Error("external model call already started");
    const call = { finished: false, usage: null as TokenUsage | null };
    for (let ancestor: RunUsage | undefined = run; ancestor; ancestor = ancestor.parent) ancestor.external.set(key, call);
  } else {
    if (!existing || existing.finished) throw new Error("external model call is missing or already completed");
    existing.usage = externalUsage(input.usage);
    existing.finished = true;
  }
}

async function completeTurn(
  profile: ModelProfile, context: Context, model: string, turn: number, run: RunUsage,
  purpose: "task" | "compaction", maxTokens: number, budget: Record<string, unknown>,
): Promise<Message> {
  const started = Date.now();
  // Persist the attempt before entering the provider. Forced cancellation
  // can prevent a reply/usage record; an unmatched start is unknown usage,
  // not a zero-token call.
  await fiber.record({ type: "model_call_started", call_index: run.own.calls + 1, purpose, turn, model,
    context: budget, max_output_tokens: maxTokens });
  let reply: Message;
  try {
    reply = await modelRequest({ operation: "complete", profile, context, purpose, maxTokens });
  } catch (error) {
    rememberUsage(run, null);
    await fiber.record({
      type: "model_call", call_index: run.own.calls, purpose, turn, model, context: budget, max_output_tokens: maxTokens, response_id: null, stop_reason: "error",
      duration_ms: Date.now() - started, usage: null,
      error: error instanceof Error ? error.message : String(error),
    });
    throw error;
  }
  const usage = tokenUsage(reply.usage);
  rememberUsage(run, usage);
  await fiber.record({
    type: "model_call", call_index: run.own.calls, purpose, turn, model, context: budget, max_output_tokens: maxTokens, response_id: reply.responseId ?? null,
    stop_reason: reply.stopReason ?? null, duration_ms: Date.now() - started, usage,
    error: reply.errorMessage ?? null,
  });
  return reply;
}

async function prepareForTurn(id: string, profile: ModelProfile, model: string, turn: number, run: RunUsage): Promise<PreparedContext> {
  const recovery = (await ctx.contextConfig()).compaction;
  let prepared: PreparedContext = await ctx.prepareContext(id, { defer_compaction: turn < run.compaction_retry_after });
  while (prepared.compaction) {
    const started = Date.now();
    const plan = prepared.compaction;
    let result: Record<string, unknown> | undefined;
    let failure = "";
    for (let attempt = 0; attempt <= recovery.max_retries; attempt++) {
      try {
        const reply = await completeTurn(profile, plan.context, model, turn, run, "compaction", plan.max_output_tokens, {
          ...prepared.budget, estimated_input_tokens: plan.estimated_input_tokens, compaction_attempt: attempt + 1,
        });
        if (["error", "aborted", "length"].includes(reply.stopReason ?? "")) throw new Error(reply.errorMessage ?? ("summary stopped with " + reply.stopReason));
        const blocks = (reply.content ?? []) as Block[];
        if (blocks.some(block => block.type === "toolCall")) throw new Error("compaction summary requested tools");
        const text = blocks.filter(block => block.type === "text").map(block => block.text ?? "").join("");
        const usage = tokenUsage(reply.usage);
        result = usage && usage.output_tokens > 0
          ? await ctx.commitCompaction(id, prepared.revision, text, usage.output_tokens)
          : await ctx.commitCompaction(id, prepared.revision, text);
        break;
      } catch (error) {
        failure = error instanceof Error ? error.message : String(error);
        await fiber.record({ type: "context_compaction", status: "failed", model, turn, attempt: attempt + 1,
          will_retry: attempt < recovery.max_retries, before_tokens: prepared.budget.estimated_input_tokens,
          duration_ms: Date.now() - started, error: failure });
        if (attempt < recovery.max_retries && recovery.retry_delay_ms > 0) {
          await new Promise(resolve => setTimeout(resolve, recovery.retry_delay_ms));
        }
      }
    }
    if (!result) {
      await ctx.cancelCompaction(id, prepared.revision);
      run.compaction_retry_after = turn + recovery.failure_cooldown_turns + 1;
      const fallback: PreparedContext = await ctx.prepareContext(id, { defer_compaction: true });
      if (!fallback.context) throw new Error("context compaction failed above the hard input budget: " + failure);
      await fiber.record({ type: "context_compaction_deferred", model, turn, reason: failure,
        retry_after_turn: run.compaction_retry_after, estimated_input_tokens: fallback.budget.estimated_input_tokens,
        input_budget_tokens: fallback.budget.input_budget_tokens });
      return fallback;
    }
    run.compaction_retry_after = 0;
    await fiber.record({ type: "context_compaction", status: "completed", model, turn,
      ...result, duration_ms: Date.now() - started, input_budget_tokens: prepared.budget.input_budget_tokens });
    prepared = await ctx.prepareContext(id);
  }
  return prepared;
}

async function runTool(call: Block, offered: string[], policy: AgentPolicy): Promise<Message> {
  const name = call.name ?? "";
  let text: string;
  let isError = false;
  let approvals: unknown[] | undefined;
  try {
    if (!offered.includes(name) || !policy.allowed_tools.includes(name)) throw new Error(`no tool named ${name}`);
    if (name === "agent" && policy.max_delegation_depth === 0) throw new Error("delegation is not allowed");
    const tool = plugins[name];
    const input = tool.wrapsInput ? call.arguments?.input : (call.arguments ?? {});
    let approval: any;
    // Bash inspects the original script (including Jev) before requesting
    // approval in its own fiber. Other model tools are gated before launch,
    // so read cannot fetch file content to construct an approval preview.
    if (name !== "bash" && typeof ctx.authorizeToolCall === "function") {
      const cwd = process.cwd();
      approval = await ctx.authorizeToolCall({ tool: name, input: input ?? null, cwd });
      if (approval?.status !== "not_required") {
        const current = await fiber.info();
        const matches = approval?.request?.schema_version === 1 && approval.request.source?.fiber === current.id
          && typeof approval.request.id === "string" && approval.request.id.startsWith(current.id + ":tool-approval:")
          && approval.request.tool === name && approval.request.cwd === cwd
          && JSON.stringify(approval.request.input) === JSON.stringify(input ?? null);
        if (!matches || !["approved", "rejected", "pending"].includes(approval?.status)) throw new Error("invalid tool approval response");
        if (approval.status === "rejected") throw new Error("tool approval rejected by user or environment");
      }
    }
    if (approval?.status === "approved") await fiber.record({ type: "tool_approved", tool: name, approval });
    const output = approval?.status === "pending" ? { status: "approval_required", approval } : await tool(input);
    text = typeof output === "string" ? output : JSON.stringify(output);
    if (output && typeof output === "object" && output.status === "approval_required") {
      approvals = Array.isArray(output.approval_requests) && output.approval_requests.length ? output.approval_requests : [output.approval ?? { tool: name, reason: "approval_required" }];
    }
  } catch (e) {
    isError = true;
    text = e instanceof Error ? e.message : String(e);
    if (name === "agent") {
      try { await fiber.record({ type: "agent_tool_error", reason: text }); } catch { /* preserve the original tool error */ }
    }
  }
  return { role: "toolResult", toolCallId: call.id, toolName: name, content: [{ type: "text", text }], isError, timestamp: Date.now(), ...(approvals ? { approval_requests: approvals } : {}) } as Message;
}

/** Offer this plugin to the model so agents can start child agents. */
export async function onLoad() {
  await plugins.context.loaded;
  await ctx.registerTool("agent");
  return { reportExternalModelCall };
}
