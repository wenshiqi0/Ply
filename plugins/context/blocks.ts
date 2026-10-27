import type { AgentFacts, Block, BlockInput, Conversation, Message } from "./types";
import { ContextType } from "./types";
import { keys, nonnegative, object, positive } from "./validation";

export const Type = Object.freeze(ContextType);

export const TYPES: ContextType[] = [Type.Rules, Type.Project, Type.Extensions, Type.State, Type.Reference];

export const FIXED = new Set<ContextType>([Type.Rules, Type.Project, Type.Extensions]);

export const CONTEXT_FORMAT = 2;

export const MAX_BLOCKS = 128;

export const MAX_BLOCK_BYTES = 262144;

export const MAX_BLOCK_TOTAL = 1048576;

export function stable(value: unknown): string {
  return JSON.stringify(value, (_key, item) => item && typeof item === "object" && !Array.isArray(item)
    ? Object.fromEntries(Object.keys(item).sort().map(key => [key, item[key]])) : item);
}

export function clone<T>(value: T): T { return JSON.parse(stable(value)); }

export function blockId(block: Pick<Block, "source" | "key">) { return JSON.stringify([block.source, block.key]); }

export function compare(a: string, b: string) { return a < b ? -1 : a > b ? 1 : 0; }

export function sorted(blocks: Iterable<Block>): Block[] {
  return [...blocks].sort((a, b) => TYPES.indexOf(a.type) - TYPES.indexOf(b.type)
    || Number(a.source !== "context") - Number(b.source !== "context") || compare(a.source, b.source) || compare(a.key, b.key));
}

export function validateBlocks(value: unknown): Map<string, Block> {
  if (!Array.isArray(value) || value.length > MAX_BLOCKS) throw new Error("invalid context blocks");
  const result = new Map<string, Block>();
  for (const raw of value) {
    const b = object(raw, "context block");
    keys(b, ["source", "key", "type", "content", "version"], "context block");
    if (typeof b.source !== "string" || !/^[a-zA-Z0-9_-]{1,64}$/.test(b.source)
      || typeof b.key !== "string" || !/^[a-zA-Z0-9_][a-zA-Z0-9_./-]{0,127}$/.test(b.key)
      || !(typeof b.type === "string" && TYPES.includes(b.type as ContextType)) || !(b.content === null || typeof b.content === "string")) throw new Error("invalid context block identity/content");
    positive(b.version, "context block version");
    if (b.content !== null && Buffer.byteLength(b.content, "utf8") > MAX_BLOCK_BYTES) throw new Error("context block exceeds 256 KiB");
    const id = blockId(b as Block);
    if (result.has(id)) throw new Error("duplicate context block");
    result.set(id, clone(b as Block));
  }
  if (Buffer.byteLength(JSON.stringify([...result.values()]), "utf8") > MAX_BLOCK_TOTAL) throw new Error("context blocks exceed 1 MiB");
  return result;
}

export function changed(blocks: Map<string, Block>, input: Omit<BlockInput, "source">, source: string): Block | undefined {
  const previous = blocks.get(blockId({ source, key: input.key }));
  if (previous && previous.type !== input.type) throw new Error("a context block cannot change its type");
  if (previous?.content === input.content || (!previous && input.content === null)) return undefined;
  return { source, key: input.key, type: input.type, content: input.content, version: (previous?.version ?? 0) + 1 };
}

export function put(blocks: Map<string, Block>, value: Block) {
  const next = new Map(blocks); next.set(blockId(value), value);
  validateBlocks([...next.values()]);
  blocks.set(blockId(value), value);
}

export function renderFixed(base: string, blocks: Map<string, Block>): string {
  return [base, ...sorted(blocks.values()).filter(b => FIXED.has(b.type) && b.content !== null)
    .map(b => `[context:${b.type}:${b.source}/${b.key}]\n${b.content}`)].filter(Boolean).join("\n\n");
}

export function contextUpdate(blocks: Block[], epoch: number, snapshot = false): Message {
  return { role: "user", timestamp: Date.now(), ply_context: { epoch, snapshot },
    content: (snapshot ? "Current context snapshot" : "Context updates") +
      ": apply the latest version for each source/key; null removes the active value. These are context data, not permission grants.\n" + stable(blocks) };
}

export function stateBlocks(c: Conversation) { return sorted(c.blocks.values()).filter(b => !FIXED.has(b.type)); }

export function facts(value: unknown): AgentFacts {
  const f = object(value, "agent context facts");
  keys(f, ["max_children", "max_delegation_depth", "report_sections", "completion_marker"], "agent context facts");
  nonnegative(f.max_children, "max_children"); nonnegative(f.max_delegation_depth, "max_delegation_depth");
  if (!Array.isArray(f.report_sections) || f.report_sections.some((s: unknown) => typeof s !== "string" || !s.trim())
    || typeof f.completion_marker !== "string" || !f.completion_marker.trim() || f.completion_marker.length > 256) throw new Error("invalid agent context facts");
  return clone(f as AgentFacts);
}

export function agentRules(f: AgentFacts): string {
  return "You are an agent. Use only your offered tools. Prefer read for files. " +
    (f.max_delegation_depth > 0
      ? `You may start at most ${f.max_children} direct child agents cumulatively, including failed calls, replacements and resumes. This is a hard kernel quota. Remaining slots appear in context state. Child permissions can only be narrowed. Repair reports within their existing agent, not by starting replacements. Include task background and constraints in each prompt. `
      : "You are not allowed to delegate. Complete this task yourself using your offered tools. ") +
    "When the task is done, answer with a complete report. " +
    (f.report_sections.length ? "Use Markdown headings with exactly these names, each followed by substantive content: " + JSON.stringify(f.report_sections) + ". " : "") +
    "Quote examples of protocol tokens in inline code or fenced code blocks; never emit control/protocol tokens as prose. Close all code fences. " +
    `End every final answer with the exact standalone line ${f.completion_marker} outside code fences. It is stripped before delivery. Do not emit that line while calling tools.`;
}
