import type { Block, Context, Message, PreparedContext, RunUsage } from "./types";
import type { ModelProfile } from "ply:models";
import { context } from "ply:plugins";
import { audit } from "./audit";
import { delegationState } from "./policy";
import { rememberUsage, tokenUsage } from "./usage";
import { request as modelRequest } from "ply:models";

export async function completeTurn(
  profile: ModelProfile, context: Context, model: string, turn: number, run: RunUsage,
  purpose: "task" | "compaction", maxTokens: number, budget: Record<string, unknown>,
): Promise<Message> {
  const started = Date.now();
  // Persist the attempt before entering the provider. Forced cancellation
  // can prevent a reply/usage record; an unmatched start is unknown usage,
  // not a zero-token call.
  await audit("model_call_started", { call_index: run.own.calls + 1, purpose, turn, model,
    context: budget, max_output_tokens: maxTokens });
  let reply: Message;
  try {
    reply = await modelRequest({ operation: "complete", profile, context, purpose, maxTokens });
  } catch (error) {
    rememberUsage(run, null);
    await audit("model_call", {
      call_index: run.own.calls, purpose, turn, model, context: budget, max_output_tokens: maxTokens, response_id: null, stop_reason: "error",
      duration_ms: Date.now() - started, usage: null,
      error: error instanceof Error ? error.message : String(error),
    });
    throw error;
  }
  const usage = tokenUsage(reply.usage);
  rememberUsage(run, usage);
  await audit("model_call", {
    call_index: run.own.calls, purpose, turn, model, context: budget, max_output_tokens: maxTokens, response_id: reply.responseId ?? null,
    stop_reason: reply.stopReason ?? null, duration_ms: Date.now() - started, usage,
    error: reply.errorMessage ?? null,
  });
  return reply;
}

export async function prepareForTurn(id: string, profile: ModelProfile, model: string, turn: number, run: RunUsage): Promise<PreparedContext> {
  const recovery = (await context.contextConfig()).compaction;
  let prepared: PreparedContext = await context.prepareContext(id, {
    defer_compaction: turn < run.compaction_retry_after, agent_state: delegationState(run) });
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
          ? await context.commitCompaction(id, prepared.revision, text, usage.output_tokens)
          : await context.commitCompaction(id, prepared.revision, text);
        break;
      } catch (error) {
        failure = error instanceof Error ? error.message : String(error);
        await audit("context_compaction", { status: "failed", model, turn, attempt: attempt + 1,
          will_retry: attempt < recovery.max_retries, before_tokens: prepared.budget.estimated_input_tokens,
          duration_ms: Date.now() - started, error: failure });
        if (attempt < recovery.max_retries && recovery.retry_delay_ms > 0) {
          await new Promise(resolve => setTimeout(resolve, recovery.retry_delay_ms));
        }
      }
    }
    if (!result) {
      await context.cancelCompaction(id, prepared.revision);
      run.compaction_retry_after = turn + recovery.failure_cooldown_turns + 1;
      const fallback: PreparedContext = await context.prepareContext(id, { defer_compaction: true });
      if (!fallback.context) throw new Error("context compaction failed above the hard input budget: " + failure);
      await audit("context_compaction", { status: "deferred", model, turn, reason: failure,
        retry_after_turn: run.compaction_retry_after, estimated_input_tokens: fallback.budget.estimated_input_tokens,
        input_budget_tokens: fallback.budget.input_budget_tokens });
      return fallback;
    }
    run.compaction_retry_after = 0;
    await audit("context_compaction", { status: "completed", model, turn,
      ...result, duration_ms: Date.now() - started, input_budget_tokens: prepared.budget.input_budget_tokens });
    prepared = await context.prepareContext(id);
  }
  return prepared;
}
