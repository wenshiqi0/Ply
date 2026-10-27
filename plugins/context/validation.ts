

export function object(value: unknown, name: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(name + " must be an object");
  return value as Record<string, unknown>;
}

export function keys(value: Record<string, unknown>, allowed: string[], name: string) {
  for (const key of Object.keys(value)) if (!allowed.includes(key)) throw new Error("unknown " + name + " field: " + key);
}

export function positive(value: unknown, name: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 1) throw new Error(name + " must be a positive safe integer");
  return value;
}

export function ratio(value: unknown, name: string): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0 || value >= 1) throw new Error(name + " must be between 0 and 1");
  return value;
}

export function nonnegative(value: unknown, name: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) throw new Error(name + " must be a non-negative safe integer");
  return value;
}

export function toolNames(value: unknown): string[] {
  if (!Array.isArray(value) || value.some(name => typeof name !== "string" || !/^[a-zA-Z0-9_-]{1,64}$/.test(name))) {
    throw new Error("allowed_tools must be an array of tool names");
  }
  return [...new Set(value)];
}
