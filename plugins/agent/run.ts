import * as checkpoints from "ply:checkpoints";
import type { AgentInput, AgentPolicy, AgentResult, Block, Message, Recovery, ReportState, RunUsage, UsageSummary } from "./types";
import { REPORT_END, reportFor, sectionNames, validateReport } from "./report";
import { audit } from "./audit";
import { delegations } from "./state";
import { combinedUsage, startUsage, tokenUsage, totalUsageSummary, usageSummary } from "./usage";
import { completeTurn, prepareForTurn } from "./model";
import { context, type ModelProfile } from "ply:plugins";
import { delegationState, integer, policyFor } from "./policy";
import { recovery } from "./recovery";
import { request as modelRequest } from "./pi";
import { runTool } from "./tools";

/** Run a task with bounded tool/delegation grants, or resume a recovery ID. */
export async function onHandle(input: AgentInput): Promise<AgentResult> {
  if (!input || typeof input !== "object") throw new Error("agent input must be an object");
  if (input.resume_id !== undefined && (typeof input.resume_id !== "string" || !input.resume_id.trim() || input.prompt !== undefined)) {
    throw new Error("resume_id must be a string; omit prompt when resuming");
  }
  if (input.resume_id === undefined && (typeof input.prompt !== "string" || !input.prompt.trim())) throw new Error("'prompt' must be a non-empty string");
  const maxTurns = input.max_turns === undefined ? undefined : integer(input.max_turns, "max_turns", 1);
  if (input.max_children !== undefined) integer(input.max_children, "max_children");
  if (input.report_sections !== undefined) sectionNames(input.report_sections, "report_sections");
  if (input.max_delegation_depth !== undefined) integer(input.max_delegation_depth, "max_delegation_depth");
  if (input.delegation !== undefined && !delegations.has(input.delegation)) throw new Error("unknown delegation; only a running parent agent can delegate");
  const parent = input.delegation === undefined ? undefined : delegations.get(input.delegation);
  let run: RunUsage | undefined;
  let id: string | undefined;
  let lease: number | undefined;
  let checkpointId = input.resume_id;
  let profile: ModelProfile | undefined;
  let policy: AgentPolicy | undefined;
  let report: ReportState = { required_sections: [], rejected_reports: 0 };
  let model: string;
  let completed = false;
  let safe = true;
  let nextTurn = 1;
  let toolCalls = 0;
  let lastReady: Recovery | undefined;
  let previousUsage: UsageSummary | undefined;
  let previousTotalUsage: UsageSummary | undefined;
  const snapshot = async (): Promise<Recovery> => ({
    format: 4, phase: "ready", cwd: process.cwd(), profile: profile!, policy: policy!,
    children_started: run!.children.started, report: { ...report, required_sections: [...report.required_sections] },
    conversation: await context.exportConversation(id!), next_turn: nextTurn, tool_calls: toolCalls,
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
    policy = policyFor(input, await context.availableTools(), parent, saved?.policy);
    report = reportFor(input, parent, saved?.report);
    run = startUsage(policy, report, parent, saved?.children_started ?? 0);
    await audit("policy", { ...policy, resumed_from: checkpointId ?? null });
    profile = saved?.profile ?? (await context.currentModel()) ?? undefined;
    if (!profile) throw new Error("no model in the context: load a model integration such as deepseek or mimo");
    model = `${profile.provider}/${profile.model}`;
    const info = await modelRequest({ operation: "describe", profile });
    const request = { model: info, allowed_tools: policy.allowed_tools,
      facts: { max_children: policy.max_children, max_delegation_depth: policy.max_delegation_depth,
        report_sections: report.required_sections, completion_marker: REPORT_END },
      ...(parent?.conversation ? { inherit: parent.conversation } : {}) };
    id = saved ? await context.restoreTask(saved.conversation, request)
      : await context.openTask({ ...request, prompt: input.prompt! });
    run.conversation = id;
    let turns = 0;
    for (let turn = nextTurn; maxTurns === undefined || turns < maxTurns; turn++) {
      nextTurn = turn;
      if (lease !== undefined) {
        lastReady = await snapshot();
        await checkpoints.update(lease, lastReady);
      }
      const prepared = await prepareForTurn(id, profile, model, turn, run);
      const requestContext = prepared.context!;
      const reply: Message = { ...(await completeTurn(profile, requestContext, model, turn, run, "task", prepared.budget.reserve_output_tokens, prepared.budget)), timestamp: Date.now() };
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
          await audit("report", { status: "rejected", model, turn, call_index: run.own.calls,
            response_id: reply.responseId ?? null, reasons: checked.errors, preview: candidate.slice(-512), rejected_reports: report.rejected_reports });
          // Keep malformed protocol text out of the next provider context.
          // Regenerate in this same agent; no extra child or slot is created.
          await context.repairReport(id, checked.errors, prepared.revision);
          nextTurn = turn + 1;
          continue;
        }
        const observed = tokenUsage(reply.usage);
        await context.appendMessages(id, [reply], { expected_revision: prepared.revision, tools_signature: prepared.tools_signature,
          ...(observed ? { context_tokens: observed.total_tokens } : {}) });
        await audit("report", { status: "validated", model, turn, call_index: run.own.calls,
          required_sections: report.required_sections, rejected_reports: report.rejected_reports });
        const delegation = delegationState(run);
        completed = true;
        return { text: checked.text, model, turns, total_turns: turn, tool_calls: toolCalls, usage: usageSummary(run.own), total_usage: totalUsageSummary(run), policy,
          delegation, report_validation: { ...report, required_sections: [...report.required_sections] },
          ...(input.resume_id ? { resumed_from: input.resume_id, previous_usage: previousUsage, previous_total_usage: previousTotalUsage } : {}) };
      }
      const observed = tokenUsage(reply.usage);
      await context.appendMessages(id, [reply], { expected_revision: prepared.revision, tools_signature: prepared.tools_signature,
        ...(observed ? { context_tokens: observed.total_tokens } : {}) });
      safe = false;
      if (lease !== undefined) await checkpoints.update(lease, { ...lastReady!, phase: "tools_pending", pending_tools: calls });
      toolCalls += calls.length;
      const offered = requestContext.tools.map(tool => tool.name);
      const results = await Promise.all(calls.map(call => runTool(call, offered, run!)));
      await context.appendToolResults(id, results);
      const approvals = results.flatMap(result => result.approval_requests ?? []);
      if (approvals.length) {
        // No extra model turns or replacement commands while user action is
        // required. A prior tools_pending lease remains non-replayable.
        return { status: "approval_required", approval_requests: approvals,
          text: "部分工具调用需要用户确认或环境审批；待审批命令尚未执行。", model, turns, total_turns: turn,
          tool_calls: toolCalls, usage: usageSummary(run.own), total_usage: totalUsageSummary(run), policy,
          delegation: delegationState(run), report_validation: { ...report, required_sections: [...report.required_sections] },
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
        await audit("checkpoint", { resume_id: checkpointId, next_turn: nextTurn, reason: message });
        message += `; resume with agent({ resume_id: "${checkpointId}" })`;
      } catch (saveError) {
        message += "; checkpoint save failed: " + (saveError instanceof Error ? saveError.message : String(saveError));
      }
    }
    throw new Error(message, { cause: error });
  } finally {
    // Also runs when the call is cancelled: the kernel gives it a bounded cleanup.
    try { if (id) await context.closeConversation(id); }
    finally {
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
