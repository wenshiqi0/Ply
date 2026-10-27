import plugins from "ply:plugins";
import type { Block, RunUsage, ToolOutcome } from "./types";
import type { ToolAuthorization } from "ply:plugins";
import { delegations } from "./state";

export function sameJson(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (!a || !b || typeof a !== "object" || typeof b !== "object" || Array.isArray(a) !== Array.isArray(b)) return false;
  const left = Object.keys(a).sort(), right = Object.keys(b).sort();
  return left.length === right.length && left.every((key, index) => key === right[index] && sameJson((a as Record<string, unknown>)[key], (b as Record<string, unknown>)[key]));
}

export async function runTool(call: Block, offered: string[], run: RunUsage): Promise<ToolOutcome> {
  const name = call.name ?? "";
  const policy = run.policy;
  let output: unknown;
  let isError = false;
  let approvals: unknown[] | undefined;
  try {
    if (!offered.includes(name) || !policy.allowed_tools.includes(name)) throw new Error(`no tool named ${name}`);
    if (name === "agent" && policy.max_delegation_depth === 0) throw new Error("delegation is not allowed");
    const tool = plugins[name];
    const input = tool.wrapsInput ? call.arguments?.input : (call.arguments ?? {});
    let approval: ToolAuthorization | undefined;
    // Bash inspects the original script (including Jev) before requesting
    // approval itself. Other model tools are gated before launch, so read
    // cannot fetch file content to construct an approval preview.
    const authorize = plugins.approval.authorizeToolCall;
    if (name !== "bash" && typeof authorize === "function") {
      const cwd = process.cwd();
      approval = await authorize({ tool: name, input: input ?? null, cwd });
      if (approval?.status !== "not_required") {
        const matches = approval?.request?.schema_version === 1 && typeof approval.request.id === "string"
          && approval.request.tool === name && approval.request.cwd === cwd && sameJson(approval.request.input, input ?? null);
        if (!approval || !matches || !["approved", "rejected", "pending"].includes(approval.status)) throw new Error("invalid tool approval response");
        if (approval.status === "rejected") throw new Error("tool approval rejected by user or environment");
      }
    }
    output = approval?.status === "pending" ? { status: "approval_required", approval }
      : name === "agent" ? await delegate(run, tool, input) : await tool(input);
    if (output && typeof output === "object" && ("status" in output ? output.status : undefined) === "approval_required") {
      const pending = output as Record<string, unknown>;
      approvals = Array.isArray(pending.approval_requests) && pending.approval_requests.length ? pending.approval_requests : [pending.approval ?? { tool: name, reason: "approval_required" }];
    }
  } catch (e) {
    isError = true;
    output = e instanceof Error ? e.message : String(e);
  }
  return { id: call.id ?? "", name, output, isError, ...(approvals ? { approval_requests: approvals } : {}) };
}

let nextDelegation = 0;

/**
 * Start a child agent of this run. The quota is checked and counted with no
 * await before the call: every agent of this plugin runs on one thread, so
 * concurrent calls cannot both pass. Failures, replacements and resumes
 * never give a slot back.
 */
async function delegate(run: RunUsage, tool: (input: unknown) => PromiseLike<unknown>, input: unknown) {
  const { limit, started } = run.children;
  if (started >= limit) throw new Error(`child limit reached for agent: ${started} started, limit ${limit}; replacements and resumes also consume this budget`);
  run.children.started++;
  const token = `delegation-${++nextDelegation}-${Math.random().toString(36).slice(2)}${Math.random().toString(36).slice(2)}`;
  delegations.set(token, run);
  try {
    return await tool({ ...(input && typeof input === "object" ? input : {}), delegation: token });
  } finally {
    delegations.delete(token);
  }
}
