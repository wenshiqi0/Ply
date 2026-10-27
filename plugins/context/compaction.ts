import type { Conversation } from "./types";
import { contextUpdate, stable, stateBlocks } from "./blocks";
import { conversation, conversations } from "./state";
import { estimatedInputTokens, exchanges, messageTokens, overflow, requestTokens, status, summaryContext, textTokens, view } from "./messages";
import { flush, updateAgentState } from "./conversation";
import { positive } from "./validation";

export function prepareContext(id: string, recovery?: { defer_compaction: boolean; agent_state?: unknown }) {
      const c = conversation(id);
      updateAgentState(c, recovery?.agent_state);
      if (!c.plan) flush(c);
      const available = c.tools;
      const budget = status(c, available);
      if (recovery?.defer_compaction && budget.estimated_input_tokens <= c.limits.input_budget_tokens) {
        c.plan = undefined;
        c.compacting = false;
        return { revision: c.revision, tools_signature: stable(available),
          budget: { ...budget, compaction_deferred: "summary_failed_cooldown" }, context: view(c, available) };
      }
      // A prepared prefix is immutable until the message revision changes.
      // Re-preparing during a model request (for example after a tool was
      // registered) must not silently enlarge the prefix that will be removed.
      if (c.plan && c.plan.revision === c.revision) {
        const planned = summaryContext(c, c.plan.end);
        return {
          revision: c.revision, tools_signature: stable(available), budget,
          compaction: {
            context: planned, max_output_tokens: c.limits.summary_max_tokens,
            estimated_input_tokens: requestTokens(planned), messages_to_summarize: c.plan.messages,
          },
        };
      }
      if (c.messages.length && c.messages[0].role !== "user") throw new Error("a conversation must start with the user task");
      const groups = exchanges(c.messages);
      const old = groups.slice(0, Math.max(0, groups.length - c.limits.keep_recent_turns));
      const triggered = c.compacting ? budget.estimated_input_tokens > c.limits.target_tokens : budget.estimated_input_tokens >= c.limits.trigger_tokens;
      if (triggered && c.limits.compaction_enabled && old.length) {
        let chosen: { end: number; context: ReturnType<typeof summaryContext>; tokens: number; after: number } | undefined;
        // Whole-exchange chunks keep each summary request inside the same
        // input budget, even when a tool batch pushed the full history over it.
        for (const group of old) {
          const candidate = summaryContext(c, group.end);
          const tokens = requestTokens(candidate);
          if (tokens > c.limits.input_budget_tokens) break;
          const remaining = textTokens(c.system) + textTokens(stable(available)) + 64 +
            (stateBlocks(c).length ? messageTokens(contextUpdate(stateBlocks(c), c.epoch + 1, true)) : 0) +
            (c.messages[0] ? messageTokens(c.messages[0]) : 0) + c.limits.summary_max_tokens + 32 +
            c.messages.slice(group.end).filter(m => !m.ply_context).reduce((n, m) => n + messageTokens(m), 0);
          chosen = { end: group.end, context: candidate, tokens, after: remaining };
          if (remaining <= c.limits.target_tokens) break;
        }
        if (!chosen) {
          c.plan = undefined;
          c.compacting = false;
          if (budget.estimated_input_tokens > c.limits.input_budget_tokens) overflow(c, budget.estimated_input_tokens,
            "a complete exchange cannot fit in a compaction request; reduce tool output or increase the context budget");
          return {
            revision: c.revision, tools_signature: stable(available),
            budget: { ...budget, compaction_deferred: "summary_exchange_exceeds_budget" }, context: view(c, available),
          };
        }
        // While a calibrated context still fits, avoid a summary that would
        // enlarge its known footprint. Above the hard cap, bounded chunks
        // may need several commits, so compare progress with one estimator.
        const comparison = budget.estimated_input_tokens > c.limits.input_budget_tokens
          ? estimatedInputTokens(c, available) : budget.estimated_input_tokens;
        if (chosen.after >= comparison) {
          c.plan = undefined;
          c.compacting = false;
          if (budget.estimated_input_tokens > c.limits.input_budget_tokens) overflow(c, budget.estimated_input_tokens,
            "retained recent exchanges prevent a useful compaction; reduce keep_recent_turns or increase the budget");
          return {
            revision: c.revision, tools_signature: stable(available),
            budget: { ...budget, compaction_deferred: "retained_history_estimate_not_smaller" }, context: view(c, available),
          };
        }
        c.compacting = true;
        c.plan = { revision: c.revision, end: chosen.end, messages: chosen.end - 1 };
        return {
          revision: c.revision,
          tools_signature: stable(available),
          budget,
          compaction: { context: chosen.context, max_output_tokens: c.limits.summary_max_tokens, estimated_input_tokens: chosen.tokens, messages_to_summarize: chosen.end - 1 },
        };
      }
      c.plan = undefined;
      c.compacting = false;
      if (budget.estimated_input_tokens > c.limits.input_budget_tokens) overflow(c, budget.estimated_input_tokens,
        c.limits.compaction_enabled ? "original task or retained recent exchanges are too large" : "automatic compaction is disabled");
      return { revision: c.revision, tools_signature: stable(available), budget, context: view(c, available) };
    }

export function commitCompaction(id: string, revision: number, summary: string, summaryTokens?: number) {
      const c = conversation(id);
      const plan = c.plan;
      if (!plan || plan.revision !== revision || c.revision !== revision) throw new Error("stale context compaction plan");
      if (typeof summary !== "string" || !summary.trim()) throw new Error("compaction returned an empty summary");
      if (summaryTokens !== undefined) positive(summaryTokens, "summary tokens");
      let timestamp = Date.now();
      for (const message of c.messages) {
        if (typeof message.timestamp === "number" && Number.isFinite(message.timestamp)) timestamp = Math.max(timestamp, message.timestamp + 1);
      }
      const candidate: Conversation = {
        ...c,
        messages: [c.messages[0], ...c.messages.slice(plan.end).filter(m => !m.ply_context)],
        pending: new Map(), epoch: c.epoch + 1,
        state_snapshot: stateBlocks(c).length ? contextUpdate(stateBlocks(c), c.epoch + 1, true) : undefined,
        revision: c.revision + 1,
        summary: { text: summary, timestamp, tokens: summaryTokens ?? textTokens(summary) },
        anchor: undefined,
        plan: undefined,
        compactions: c.compactions + 1,
      };
      const available = c.tools;
      // Compare both sides with the same estimator. A provider usage anchor
      // may be far below the byte estimate; mixing them rejects real progress.
      const before = estimatedInputTokens(c, available);
      const after = estimatedInputTokens(candidate, available);
      if (after >= before) throw new Error("compaction did not reduce the context; old history was retained");
      // A bounded prefix may need multiple summaries before reaching the
      // hard cap. Each commit removes at least one complete exchange.
      conversations.set(id, candidate);
      return {
        revision: candidate.revision,
        before_tokens: before,
        after_tokens: after,
        messages_summarized: plan.messages,
        messages_retained: candidate.messages.length,
        target_tokens: c.limits.target_tokens,
        target_reached: after <= c.limits.target_tokens,
        compactions: candidate.compactions,
      };
    }

export function cancelCompaction(id: string, revision: number) {
      const c = conversation(id);
      if (!c.plan || c.revision !== revision || c.plan.revision !== revision) throw new Error("stale compaction recovery");
      c.plan = undefined;
      c.compacting = false;
    }
