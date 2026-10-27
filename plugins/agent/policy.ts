import type { AgentInput, AgentPolicy, RunUsage } from "./types";

export function integer(value: unknown, name: string, minimum = 0): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < minimum) throw new Error(name + " must be a safe integer >= " + minimum);
  return value;
}

export function policyFor(input: AgentInput, available: string[], parent?: RunUsage, saved?: AgentPolicy): AgentPolicy {
  if (parent && parent.policy.max_delegation_depth === 0) throw new Error("the parent agent is not allowed to delegate");
  const ceiling = Math.min(parent ? parent.policy.max_delegation_depth - 1 : (input.max_delegation_depth ?? saved?.max_delegation_depth ?? 1),
    saved?.max_delegation_depth ?? Number.MAX_SAFE_INTEGER);
  const depth = input.max_delegation_depth === undefined ? ceiling : integer(input.max_delegation_depth, "max_delegation_depth");
  if (depth > ceiling) throw new Error("a child/resumed agent cannot increase delegation depth");
  const base = available.filter(name => (!parent || parent.policy.allowed_tools.includes(name) && name !== "memory") && (!saved || saved.allowed_tools.includes(name)));
  let names = base;
  if (input.allowed_tools !== undefined) {
    if (!Array.isArray(input.allowed_tools) || input.allowed_tools.some(name => typeof name !== "string" || !base.includes(name))) {
      throw new Error("allowed_tools contains an unavailable or unauthorized tool");
    }
    names = [...new Set(input.allowed_tools)];
  }
  const defaultChildren = integer(Number(process.env.PLY_AGENT_MAX_CHILDREN ?? 2), "default max_children");
  const childCeiling = Math.min(parent ? parent.policy.max_children : (input.max_children ?? saved?.max_children ?? defaultChildren),
    saved?.max_children ?? Number.MAX_SAFE_INTEGER);
  const children = input.max_children === undefined ? childCeiling : integer(input.max_children, "max_children");
  if (children > childCeiling) throw new Error("a child/resumed agent cannot increase max_children");
  names = names.filter(name => name !== "agent" || depth > 0 && children > 0).sort();
  return { allowed_tools: names, max_delegation_depth: names.includes("agent") ? depth : 0,
    max_children: names.includes("agent") ? children : 0 };
}

/** The direct-child quota, as results and context report it. */
export function delegationState(run: RunUsage) {
  const { limit, started } = run.children;
  return { limit, started, remaining: Math.max(0, limit - started) };
}
