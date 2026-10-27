import type { ContextConfig, Limits, ModelLimits } from "./types";
import { keys, nonnegative, object, positive, ratio } from "./validation";

export const DEFAULT_CONFIG: ContextConfig = {
  max_context_tokens: 131_072,
  reserve_output_tokens: 16_384,
  compaction: { enabled: true, trigger_ratio: 0.8, target_ratio: 0.5, keep_recent_turns: 4, summary_max_tokens: 2048,
    max_retries: 1, retry_delay_ms: 500, failure_cooldown_turns: 2 },
};

export function readConfig(): ContextConfig {
  const raw = object(process.env.PLY_CONTEXT_CONFIG ? JSON.parse(process.env.PLY_CONTEXT_CONFIG) : {}, "context config");
  keys(raw, ["max_context_tokens", "reserve_output_tokens", "compaction"], "context config");
  const compact = raw.compaction === undefined ? {} : object(raw.compaction, "compaction");
  keys(compact, ["enabled", "trigger_ratio", "target_ratio", "keep_recent_turns", "summary_max_tokens", "max_retries", "retry_delay_ms", "failure_cooldown_turns"], "compaction");
  const defaults = DEFAULT_CONFIG.compaction;
  const merged = { ...DEFAULT_CONFIG, ...raw, compaction: { ...defaults, ...compact } };
  const enabled = merged.compaction.enabled;
  if (typeof enabled !== "boolean") throw new Error("compaction.enabled must be a boolean");
  const config: ContextConfig = {
    max_context_tokens: positive(merged.max_context_tokens, "max_context_tokens"),
    reserve_output_tokens: positive(merged.reserve_output_tokens, "reserve_output_tokens"),
    compaction: {
      enabled,
      trigger_ratio: ratio(merged.compaction.trigger_ratio, "compaction.trigger_ratio"),
      target_ratio: ratio(merged.compaction.target_ratio, "compaction.target_ratio"),
      keep_recent_turns: positive(merged.compaction.keep_recent_turns, "compaction.keep_recent_turns"),
      summary_max_tokens: positive(merged.compaction.summary_max_tokens, "compaction.summary_max_tokens"),
      max_retries: nonnegative(merged.compaction.max_retries, "compaction.max_retries"),
      retry_delay_ms: nonnegative(merged.compaction.retry_delay_ms, "compaction.retry_delay_ms"),
      failure_cooldown_turns: nonnegative(merged.compaction.failure_cooldown_turns, "compaction.failure_cooldown_turns"),
    },
  };
  if (config.compaction.max_retries > 10 || config.compaction.retry_delay_ms > 60_000 || config.compaction.failure_cooldown_turns > 100) {
    throw new Error("compaction recovery exceeds its limit (10 retries, 60000 ms delay, 100 cooldown turns)");
  }
  if (config.reserve_output_tokens >= config.max_context_tokens) throw new Error("reserve_output_tokens must be smaller than max_context_tokens");
  if (config.compaction.target_ratio >= config.compaction.trigger_ratio) throw new Error("compaction.target_ratio must be smaller than compaction.trigger_ratio");
  if (config.compaction.summary_max_tokens > config.reserve_output_tokens) throw new Error("compaction.summary_max_tokens must not exceed reserve_output_tokens");
  return config;
}

export const config = readConfig();

export function limitsFor(info?: ModelLimits): Limits {
  if (info) {
    positive(info.contextWindow, "model contextWindow");
    positive(info.maxTokens, "model maxTokens");
  }
  const window = Math.min(config.max_context_tokens, info?.contextWindow ?? config.max_context_tokens);
  const reserve = Math.min(config.reserve_output_tokens, info?.maxTokens ?? config.reserve_output_tokens);
  if (reserve >= window) throw new Error("output reserve leaves no input budget in the model context window");
  const input = window - reserve;
  const trigger = Math.floor(input * config.compaction.trigger_ratio);
  const target = Math.floor(input * config.compaction.target_ratio);
  const summary = Math.min(config.compaction.summary_max_tokens, reserve);
  if (config.compaction.enabled && (target <= summary + 32 || trigger <= target)) {
    throw new Error("compaction target must leave room for the summary and be below the trigger after reserving output");
  }
  return {
    configured_context_tokens: config.max_context_tokens,
    model_context_tokens: info?.contextWindow ?? null,
    max_context_tokens: window,
    reserve_output_tokens: reserve,
    input_budget_tokens: input,
    trigger_tokens: trigger,
    target_tokens: target,
    summary_max_tokens: summary,
    keep_recent_turns: config.compaction.keep_recent_turns,
    compaction_enabled: config.compaction.enabled,
  };
}
