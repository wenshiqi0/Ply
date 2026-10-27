import * as events from "ply:events";
import type { BashRequest, Result } from "./types";
import { configureReview, declareAudit, review, stats } from "./review";
import { hasCredentials } from "ply:http";
import { readConfig } from "./config";

/** Jev is a bash safety subscriber, not a model-visible execution tool. */
export async function onLoad() {
  const config = readConfig();
  configureReview(config);
  await declareAudit();
  await events.on<BashRequest, Result>("bash.beforeExecute", review);
  return { jevStatus: () => ({ enabled: true, configured: hasCredentials("jev"), model: config.model, base_url: config.base,
    thresholds: { allow_max_risk: config.allowMax, deny_min_risk: config.denyMin },
    ...stats, unfinished_checks: stats.checks - stats.allowed - stats.denied - stats.review, complete: stats.missing_calls === 0, cache_read_tokens: null, cache_write_tokens: null, cache_hit_rate: null }) };
}
