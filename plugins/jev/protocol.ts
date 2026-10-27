import type { Risk, Usage } from "./types";
import { RISKS } from "./questions";

export function count(value: unknown): value is number { return typeof value === "number" && Number.isSafeInteger(value) && value >= 0; }

export function readUsage(raw: unknown): Usage | null {
  const value = raw && typeof raw === "object" ? raw as Record<string, unknown> : null;
  if (!value || !count(value.input_tokens) || !count(value.output_tokens)) return null;
  const total = value.input_tokens + value.output_tokens;
  if (!count(total)) return null;
  // TypeSafe reports totals, not cache/reasoning breakdowns. Unknown is not zero.
  return { input_tokens: value.input_tokens, output_tokens: value.output_tokens, total_tokens: total,
    uncached_input_tokens: null, cache_read_tokens: null, cache_write_tokens: null, reasoning_tokens: null, cache_hit_rate: null };
}

export function assessment(raw: unknown): { model: string; probabilities: Record<Risk, number> } {
  const value = raw && typeof raw === "object" ? raw as Record<string, unknown> : null;
  if (!value || typeof value.model !== "string" || !value.model.trim() || value.model.length > 128 || !readUsage(value.usage)) throw new Error("invalid_response");
  const answers = value.answers;
  if (!answers || typeof answers !== "object" || Array.isArray(answers) || Object.keys(answers).length !== RISKS.length || Object.keys(answers).some(key => !RISKS.includes(key as Risk))) throw new Error("invalid_answers");
  const probabilities = {} as Record<Risk, number>;
  for (const key of RISKS) {
    const rawAnswer = (answers as Record<string, unknown>)[key];
    const answer = rawAnswer && typeof rawAnswer === "object" ? rawAnswer as Record<string, unknown> : null;
    if (!answer || answer.type !== "noul" || typeof answer.noul !== "number" || !Number.isFinite(answer.noul) || answer.noul < 0 || answer.noul > 1) throw new Error("invalid_answers");
    probabilities[key] = answer.noul;
  }
  return { model: value.model, probabilities };
}
