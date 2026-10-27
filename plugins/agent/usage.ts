import type { AgentPolicy, ReportState, RunUsage, TokenUsage, UsageCounts, UsageSummary } from "./types";
import { integer } from "./policy";

export function emptyUsage(): UsageCounts {
  return {
    calls: 0, reported_calls: 0, missing_calls: 0,
    input_tokens: 0, uncached_input_tokens: 0, output_tokens: 0,
    cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0, total_tokens: 0,
  };
}

export function combinedUsage(previous: UsageSummary | undefined, current: UsageSummary): UsageSummary {
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

/** A run's usage also counts toward every run that delegated to it, even when it fails. */
export function startUsage(policy: AgentPolicy, report: ReportState, parent: RunUsage | undefined, childrenStarted: number): RunUsage {
  return { own: emptyUsage(), total: emptyUsage(), parent, policy, report, compaction_retry_after: 0,
    children: { limit: policy.max_children, started: childrenStarted } };
}

export function isTokenCount(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

export function tokenUsage(value: unknown): TokenUsage | null {
  if (!value || typeof value !== "object") return null;
  const raw = value as Record<string, unknown>;
  if (!isTokenCount(raw.input) || !isTokenCount(raw.output) || !isTokenCount(raw.cacheRead) || !isTokenCount(raw.cacheWrite)) return null;
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

export function nullableSum(left: number | null, right: number | null): number | null {
  return left === null || right === null ? null : left + right;
}

export function addUsage(counts: UsageCounts, usage: TokenUsage | null) {
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

export function rememberUsage(run: RunUsage, usage: TokenUsage | null) {
  addUsage(run.own, usage);
  for (let ancestor: RunUsage | undefined = run; ancestor; ancestor = ancestor.parent) addUsage(ancestor.total, usage);
}

export function usageSummary(counts: UsageCounts): UsageSummary {
  const complete = counts.missing_calls === 0;
  return {
    ...counts,
    complete,
    // Weight by input tokens, not by the number of calls. A partial run
    // cannot claim a hit rate for all of its calls.
    cache_hit_rate: complete && counts.input_tokens > 0 && counts.cache_read_tokens !== null ? counts.cache_read_tokens / counts.input_tokens : null,
  };
}

export function totalUsageSummary(run: RunUsage): UsageSummary {
  return usageSummary(run.total);
}
