import { context } from "ply:plugins";
import path from "node:path";
import { exec } from "ply:sh";
import * as events from "ply:events";

let beforeExecute: string;
type Decision = boolean | null | { allow?: boolean; reason?: string; requires_approval?: boolean };

/**
 * Run a script with Ply sh in the mounted workspace. Supports its shell
 * syntax, builtins and bundled utilities (cat, ls, grep, sed, find, sort,
 * head, tail, wc, du, etc.), then host executable files found through PATH.
 * Host programs need a disk cwd and use the real host filesystem; internal
 * utilities and redirections use mounted IO. Missing commands return 127.
 * Scripts are never forwarded to a system shell. Awaits bash.beforeExecute listeners
 * before executing any part of the script.
 */
export async function onHandle(input: {
  /** The script interpreted by Ply sh. */
  command: string;
  /** Working directory (default: the workspace). */
  cwd?: string;
  /** Inspection + execution deadline; approval waiting has its own deadline. */
  timeout_ms?: number;
}): Promise<{
  exit_code: number | null;
  signal: string | null;
  timed_out: boolean;
  stdout: string;
  stderr: string;
  status?: "approval_required";
  approval?: unknown;
}> {
  if (typeof input?.command !== "string" || input.command === "") throw new Error("'command' must be a non-empty string");
  if (input.cwd !== undefined && typeof input.cwd !== "string") throw new Error("'cwd' must be a string");
  const timeout = input.timeout_ms ?? 120000;
  if (!Number.isSafeInteger(timeout) || timeout <= 0) throw new Error("'timeout_ms' must be a positive safe integer");
  const started = Date.now();
  const request = { command: input.command, cwd: path.resolve(input.cwd ?? "."), timeout_ms: timeout };
  const results = await events.emit<typeof request, Decision>(beforeExecute, request, { timeout_ms: timeout });
  const assessments: unknown[] = [];
  for (const result of results) {
    if (result && typeof result === "object" && result.requires_approval === true) assessments.push(result);
    else if (result === false || (result && typeof result === "object" && result.allow === false)) {
      throw new Error("bash.beforeExecute denied: " + (result && typeof result === "object" && typeof result.reason === "string" ? result.reason : "command rejected by a listener"));
    }
  }
  // Human/service waiting does not consume the execution budget.
  const remaining = timeout - (Date.now() - started);
  if (remaining <= 0) return { exit_code: null, signal: null, timed_out: true, stdout: "", stderr: "" };
  let approval: any;
  if (assessments.length) {
    const current = await fiber.info();
    const pending = { schema_version: 1, id: current.id + ":approval", tool: "bash", command: request.command, cwd: request.cwd, assessments,
      source: { fiber: current.id, parent: current.parent }, requested_at: Date.now() };
    approval = typeof ctx.requestCommandApproval === "function"
      ? await ctx.requestCommandApproval(pending)
      : { status: "pending", request: pending, responses: [], reason: "approval_provider_missing" };
    const matches = approval?.request?.id === pending.id && approval.request.command === request.command && approval.request.cwd === request.cwd;
    if (!matches || !["approved", "rejected", "pending"].includes(approval?.status)) {
      approval = { status: "pending", request: pending, responses: [], reason: "invalid_approval_response" };
    }
    if (approval.status === "rejected") throw new Error("command approval rejected by user or environment");
    if (approval.status !== "approved") {
      await fiber.record({ type: "command_approval_required", approval });
      return { status: "approval_required", approval, exit_code: null, signal: null, timed_out: false, stdout: "", stderr: "" };
    }
    await fiber.record({ type: "command_approved", approval });
  }
  const output = await exec({ ...request, timeout_ms: remaining });
  return approval ? { ...output, approval } : output;
}

/** Declare the tool's event and offer the tool to the model. */
export async function onLoad() {
  beforeExecute = await events.define("beforeExecute");
  await context.loaded;
  await context.registerTool("bash");
}
