import type { Config } from "./types";

export function integerEnv(name: string, fallback: number, minimum: number, maximum: number): number {
  const raw = process.env[name];
  const value = raw === undefined ? fallback : Number(raw);
  if ((raw !== undefined && !raw.trim()) || !Number.isSafeInteger(value) || value < minimum || value > maximum) throw new Error(`${name} must be an integer in ${minimum}..${maximum}`);
  return value;
}

export function probabilityEnv(name: string, fallback: number): number {
  const raw = process.env[name];
  const value = raw === undefined ? fallback : Number(raw);
  if ((raw !== undefined && !raw.trim()) || !Number.isFinite(value) || value < 0 || value > 1) throw new Error(`${name} must be a number in 0..1`);
  return value;
}

export function readConfig(): Config {
  const base = (process.env.JEV_BASE_URL ?? "https://api.typesafe.ai/v1").trim().replace(/\/+$/, "");
  if (!/^https?:\/\/[^/?#\s]+(?:\/[^?#\s]*)?$/.test(base) || /^https?:\/\/[^/]*@/.test(base)) throw new Error("JEV_BASE_URL must be an http/https base URL without credentials, query or fragment");
  const model = (process.env.JEV_MODEL ?? "jev-latest").trim();
  if (!model || model.length > 128 || /\s/.test(model)) throw new Error("JEV_MODEL must be a non-empty model ID or alias");
  const allowMax = probabilityEnv("JEV_ALLOW_MAX_RISK", 0.10);
  const denyMin = probabilityEnv("JEV_DENY_MIN_RISK", 0.90);
  if (allowMax >= denyMin) throw new Error("JEV_ALLOW_MAX_RISK must be below JEV_DENY_MIN_RISK");
  return { base, model, timeout: integerEnv("JEV_TIMEOUT_MS", 10000, 1, 120000), maxStateBytes: integerEnv("JEV_MAX_STATE_BYTES", 16384, 256, 16384), allowMax, denyMin, initialCwd: process.cwd() };
}
