import * as events from "ply:events";
import path from "node:path";
import type { BashInput, BashResult, Decision } from "./types";
import type { CommandAuthorization } from "ply:plugins";
import plugins, { context } from "ply:plugins";
import { exec } from "ply:sh";

let beforeExecute: string;

// The model's commands never see API keys: the plugin environment carries
// them only for model calls (RFC 0013).
function commandEnv(): Record<string, string | undefined> {
  return Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.endsWith("_API_KEY")));
}

/**
 * Run a script with Ply sh in the mounted workspace. Supports its shell
 * syntax, builtins and bundled utilities (cat, ls, grep, sed, find, sort,
 * head, tail, wc, du, etc.), then host executable files found through PATH.
 * Host programs need a disk cwd and use the real host filesystem; internal
 * utilities and redirections use mounted IO. Missing commands return 127.
 * Scripts are never forwarded to a system shell. Awaits bash.beforeExecute listeners
 * before executing any part of the script.
 */
export async function onHandle(input: BashInput): Promise<BashResult> {
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
  let approval: CommandAuthorization | undefined;
  if (assessments.length) {
    const pending: PlyApprovalRequest = { schema_version: 1, id: "unsubmitted", tool: "bash", command: request.command, cwd: request.cwd, assessments, requested_at: Date.now() };
    const requestApproval = plugins.approval.requestCommandApproval;
    approval = typeof requestApproval === "function"
      ? await requestApproval({ tool: "bash", command: request.command, cwd: request.cwd, assessments })
      : { status: "pending", request: pending, responses: [], reason: "approval_provider_missing" };
    const matches = typeof approval?.request?.id === "string" && approval.request.command === request.command && approval.request.cwd === request.cwd;
    if (!approval || !matches || !["approved", "rejected", "pending"].includes(approval.status)) {
      approval = { status: "pending", request: pending, responses: [], reason: "invalid_approval_response" };
    }
    if (approval.status === "rejected") throw new Error("command approval rejected by user or environment");
    if (approval.status !== "approved") {
      return { status: "approval_required", approval, exit_code: null, signal: null, timed_out: false, stdout: "", stderr: "" };
    }
  }
  const output = await exec({ ...request, timeout_ms: remaining, env: commandEnv() });
  return approval ? { ...output, approval } : output;
}

/** Declare the tool's event and offer the tool to the model. */
export async function onLoad() {
  beforeExecute = await events.define("beforeExecute");
  await context.registerTool("bash");
}
