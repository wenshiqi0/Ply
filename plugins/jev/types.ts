import type { QUESTIONS } from "./questions";

export type Risk = keyof typeof QUESTIONS;

export type BashRequest = { command: string; cwd: string; timeout_ms: number };

export type Usage = {
  input_tokens: number; output_tokens: number; total_tokens: number;
  uncached_input_tokens: null; cache_read_tokens: null; cache_write_tokens: null;
  reasoning_tokens: null; cache_hit_rate: null;
};

export type Result = {
  schema_version: 1; check_id: string; allow: boolean; requires_approval: boolean;
  decision: "allow" | "deny" | "review"; risk_level: "low" | "high" | "uncertain" | "unknown";
  risk_probabilities: Record<Risk, number> | null; reason_codes: string[]; reason: string;
  requested_model: string; model: string | null;
  status: "completed" | "failed"; usage: Usage | null; duration_ms: number;
};

export type Config = { base: string; model: string; timeout: number; maxStateBytes: number; allowMax: number; denyMin: number; initialCwd: string };
