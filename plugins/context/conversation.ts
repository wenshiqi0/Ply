import type { AgentFacts, Conversation, Message, ModelLimits } from "./types";
import { CONTEXT_FORMAT, FIXED, Type, blockId, changed, clone, contextUpdate, facts, put, renderFixed, sorted, stable, stateBlocks, validateBlocks } from "./blocks";
import { conversation, conversations } from "./state";
import { exchanges, trimmed, view } from "./messages";
import { inheritedBlocks, refreshProviders } from "./providers";
import { keys, nonnegative, object, positive, toolNames } from "./validation";
import { limitsFor } from "./config";
import { toolList } from "./tools";

export let opened = 0;

export function flush(c: Conversation) {
  if (!c.pending.size || !c.messages.length) return;
  exchanges(c.messages); // Never put updates between a tool call and its results.
  c.messages.push(contextUpdate(sorted(c.pending.values()), c.epoch));
  c.pending.clear(); c.revision++; c.plan = undefined;
}

export async function open(base: string, info?: ModelLimits, allowed?: string[], f?: AgentFacts, query = "", inherit?: string): Promise<string> {
  const parent = inherit == null ? undefined : conversation(inherit);
  const blocks = inheritedBlocks(parent);
  if (f) put(blocks, { source: "context", key: "workspace", type: Type.Project, content: "Current working directory: " + process.cwd(), version: 1 });
  const id = "conversation-" + (++opened);
  const c: Conversation = { base, system: renderFixed(base, blocks), blocks, pending: new Map(), tools: clone(toolList(allowed)), inherited: parent !== undefined, facts: f,
    epoch: 1, messages: [], revision: 0, limits: limitsFor(info), compactions: 0, compacting: false,
    allowed_tools: allowed === undefined ? undefined : toolNames(allowed) };
  for (const b of stateBlocks(c)) c.pending.set(blockId(b), b);
  conversations.set(id, c);
  if (!parent) await refreshProviders(id,query);
  return id;
}

export function append(c: Conversation, messages: Message[], observation?: { expected_revision: number; context_tokens?: number; tools_signature?: string }) {
  if (observation && observation.expected_revision !== c.revision) throw new Error("stale context revision when appending model response");
  if (observation?.context_tokens !== undefined) positive(observation.context_tokens, "observed context_tokens");
  if (observation?.context_tokens !== undefined && (messages.length !== 1 || messages[0].role !== "assistant" || typeof observation.tools_signature !== "string")) throw new Error("a usage anchor requires one assistant response and the request tools_signature");
  if (messages.length) {
    c.messages.push(...messages.map(trimmed)); c.revision++; c.plan = undefined;
    if (observation?.context_tokens !== undefined) c.anchor = observation.tools_signature === stable(c.tools)
      ? { length: c.messages.length, tokens: observation.context_tokens, tools: observation.tools_signature } : undefined;
  }
  return view(c);
}

export function updateAgentState(c: Conversation, value: unknown) {
  if (value === undefined) return;
  const v = object(value, "agent state"); keys(v, ["limit", "started", "remaining"], "agent state");
  for (const key of ["limit", "started", "remaining"]) nonnegative(v[key], "agent state " + key);
  if (v.remaining !== Math.max(0, nonnegative(v.limit, "agent state limit") - nonnegative(v.started, "agent state started"))) throw new Error("invalid agent quota state");
  const b = changed(c.blocks, { type: Type.State, key: "delegation", content: stable(v) }, "context");
  if (b) { put(c.blocks, b); c.pending.set(blockId(b), b); }
}

export function restore(snapshot: unknown, info: ModelLimits, allowed: string[], baseOverride?: string, f?: AgentFacts, inherit?: string): string {
  if (inherit != null) conversation(inherit);
  const data = object(snapshot, "conversation checkpoint");
  if (data.format !== CONTEXT_FORMAT) throw new Error("unsupported context checkpoint format: expected 2; old contexts cannot be resumed");
  if (typeof data.base !== "string" || typeof data.system !== "string" || !Array.isArray(data.messages) || !data.messages.length || data.messages[0]?.role !== "user") throw new Error("invalid checkpoint context");
  const blocks = validateBlocks(data.blocks), pending = validateBlocks(data.pending);
  for (const [id, b] of pending) if (stable(blocks.get(id)) !== stable(b) || FIXED.has(b.type)) throw new Error("invalid pending context update");
  const messages: Message[] = data.messages.map((message: unknown) => {
    const m = object(message, "checkpoint message");
    if (!(typeof m.role === "string" && ["user", "assistant", "toolResult"].includes(m.role)) || !(typeof m.content === "string" || Array.isArray(m.content))) throw new Error("invalid checkpoint message role/content");
    if (m.timestamp !== undefined && (typeof m.timestamp !== "number" || !Number.isFinite(m.timestamp) || m.timestamp < 0)) throw new Error("invalid checkpoint timestamp");
    return trimmed(m as Message);
  });
  exchanges(messages);
  if (renderFixed(data.base, blocks) !== data.system) throw new Error("checkpoint fixed prefix does not match its blocks");
  const tools = clone(toolList(allowed)), epoch = positive(data.epoch, "context epoch");
  const savedFacts = data.facts == null ? undefined : facts(data.facts);
  if (!Array.isArray(data.tools)) throw new Error("invalid checkpoint tools");
  let summary: Conversation["summary"];
  if (data.summary !== null) {
    const v = object(data.summary, "checkpoint summary");
    if (typeof v.text !== "string" || !v.text.trim()) throw new Error("invalid checkpoint summary");
    summary = { text: v.text, tokens: positive(v.tokens, "checkpoint summary tokens"), timestamp: nonnegative(v.timestamp, "checkpoint summary timestamp") };
  }
  let stateSnapshot: Message | undefined;
  if (data.state_snapshot !== null) {
    const m = object(data.state_snapshot, "context state snapshot");
    if (m.role !== "user" || typeof m.content !== "string" || (!m.ply_context || typeof m.ply_context !== "object" || !("snapshot" in m.ply_context) || m.ply_context.snapshot !== true)) throw new Error("invalid context state snapshot");
    stateSnapshot = clone(m as Message);
  }
  const base = baseOverride ?? data.base;
  const c: Conversation = { base, system: renderFixed(base, blocks), blocks, pending, tools, inherited: inherit != null, facts: f ?? savedFacts,
    epoch, messages, state_snapshot: stateSnapshot, revision: 1, limits: limitsFor(info), allowed_tools: toolNames(allowed),
    compactions: nonnegative(data.compactions, "checkpoint compactions"), compacting: false, summary };
  if (base !== data.base || stable(data.tools) !== stable(tools)) c.epoch++;
  const id = "conversation-" + (++opened); conversations.set(id, c);
  return id;
}
