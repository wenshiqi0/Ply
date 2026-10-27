import type { BlockInput, Message, ModelLimits, ModelProfile, TaskInput } from "./types";
import { CONTEXT_FORMAT, FIXED, TYPES, Type, agentRules, blockId, changed, clone, facts, put, sorted } from "./blocks";
import { append, open, restore } from "./conversation";
import { cancelCompaction, commitCompaction, prepareContext } from "./compaction";
import { config } from "./config";
import { conversation, conversations, offered, providers, refreshes, templates } from "./state";
import { exchanges, prefixFingerprint } from "./messages";
import { keys, object, toolNames } from "./validation";
import { refreshProviders } from "./providers";
import { toolList } from "./tools";

let model: ModelProfile | null = null;


function setBlock(input: BlockInput) {
  const raw = object(input, "context input"); keys(raw, ["source", "type", "key", "content", "conversation", "provider"], "context input");
  if (typeof input.source !== "string" || !/^[a-zA-Z0-9_-]{1,64}$/.test(input.source) || input.source === "context"
    || !TYPES.includes(input.type) || typeof input.key !== "string" || !/^[a-zA-Z0-9_][a-zA-Z0-9_./-]{0,127}$/.test(input.key)
    || !(input.content === null || typeof input.content === "string") || (input.conversation !== undefined && typeof input.conversation !== "string")) throw new Error("invalid context input");
  const source = input.source;
  const isTemplate = input.conversation === undefined;
  const declared = providers.get(blockId({source,key:input.key}));
  if (declared && declared.type !== input.type) throw new Error("a context provider cannot change its type");
  if (input.provider !== undefined) {
    if (!isTemplate || FIXED.has(input.type) || input.content !== null || typeof input.provider !== "string" || !/^[a-zA-Z_][a-zA-Z0-9_]{0,63}$/.test(input.provider)) throw new Error("context providers require a template, State/Reference, a method name, and null content");
    const id = blockId({source,key:input.key}), previous = providers.get(id);
    if (templates.has(id) && templates.get(id)!.type !== input.type) throw new Error("a context block cannot change its type");
    providers.set(id, {source,key:input.key,type:input.type,method:input.provider});
    return { changed:previous?.method !== input.provider, scope:"template" };
  }
  const c = isTemplate ? undefined : conversation(input.conversation!);
  const blocks = c?.blocks ?? templates;
  const value = changed(blocks, input, source);
  if (!value) return { changed: false };
  if (c && FIXED.has(input.type)) throw new Error("fixed context is frozen; change the template for a new conversation");
  put(blocks, value);
  if (c) c.pending.set(blockId(value), value);
  return { changed: true, version: value.version, scope: c ? "conversation" : "template" };
}

/** Model selection, tool registration, conversations, and compaction policy. */
export async function onLoad() {
  return {
    registerTool(name: string) { offered.add(name); },
    useModel(profile: ModelProfile) { model = profile; },
    currentModel: () => model,
    contextConfig: () => config,
    availableTools: () => toolList().map(tool => tool.name),
    Type,
    set: setBlock,
    inspect(id: string) {
      const c = conversation(id);
      return { epoch: c.epoch, fixed_prefix_fingerprint: prefixFingerprint(c), revision: c.revision,
        fixed_order: sorted(c.blocks.values()).filter(b => FIXED.has(b.type) && b.content !== null).map(b => ({ type: b.type, source: b.source, key: b.key })),
        tool_order: c.tools.map(t => t.name), pending_updates: sorted(c.pending.values()).map(b => ({ type: b.type, source: b.source, key: b.key, version: b.version })) };
    },
    /** A conversation's blocks, or without an id the templates. */
    blocks(id?: string) {
      return clone(sorted((id === undefined ? templates : conversation(id).blocks).values()));
    },
    openConversation(system: string, info?: ModelLimits, allowed?: string[]) {
      if (typeof system !== "string") throw new Error("system must be a string");
      return open(system, info, allowed);
    },
    async openTask(input: TaskInput) {
      const value = object(input, "task context"); keys(value, ["prompt", "model", "allowed_tools", "facts", "inherit"], "task context");
      if (typeof input.prompt !== "string" || !input.prompt.trim()) throw new Error("task prompt must not be empty");
      const f = facts(input.facts), allowed = toolNames(input.allowed_tools);
      const id = await open(agentRules(f), input.model, allowed, f, input.prompt, input.inherit);
      append(conversation(id), [{ role: "user", content: input.prompt, timestamp: Date.now() }]);
      return id;
    },
    appendMessages(id: string, messages: Message[], observation?: { expected_revision: number; context_tokens?: number; tools_signature?: string }) {
      return append(conversation(id), messages, observation);
    },
    /** A complete tool batch; then Reference providers refresh (memory may have changed). */
    async appendToolResults(id: string, outcomes: { id: string; name: string; output: unknown; isError: boolean; approval_requests?: unknown[] }[]) {
      const c = conversation(id);
      if (!Array.isArray(outcomes) || outcomes.some(r => typeof r.id !== "string" || !r.id || typeof r.name !== "string" || typeof r.isError !== "boolean")) throw new Error("invalid tool outcomes");
      const result = append(c, outcomes.map(r => ({ role: "toolResult", toolCallId: r.id, toolName: r.name, isError: r.isError, timestamp: Date.now(),
        content: [{ type: "text", text: typeof r.output === "string" ? r.output : JSON.stringify(r.output) }],
        ...(r.approval_requests ? { approval_requests: r.approval_requests } : {}) })));
      if (!c.inherited) await refreshProviders(id, typeof c.messages[0]?.content === "string" ? c.messages[0].content : "");
      return result;
    },
    repairReport(id: string, errors: string[], revision: number) {
      const c = conversation(id);
      if (!c.facts || !Array.isArray(errors) || errors.some(e => typeof e !== "string")) throw new Error("invalid report repair context");
      const f = c.facts;
      return append(c, [{ role: "user", timestamp: Date.now(), content: "Your last final report was rejected: " + errors.join(", ") + ". " +
        "Regenerate the complete answer from the retained evidence in this same agent. Keep it concise enough to fit the output budget. Do not delegate a replacement. " +
        (f.report_sections.length ? "Required Markdown headings: " + JSON.stringify(f.report_sections) + ". " : "") +
        "Close code fences, omit protocol tokens from prose, and finish with the exact standalone line " + f.completion_marker + ".", ply_report_repair: true }], { expected_revision: revision });
    },
    prepareContext,
    commitCompaction,
    cancelCompaction,
    exportConversation(id: string) {
      const c = conversation(id);
      exchanges(c.messages); // A checkpoint may never replay a pending tool batch.
      return clone({ format: CONTEXT_FORMAT, base: c.base, system: c.system, blocks: sorted(c.blocks.values()), pending: sorted(c.pending.values()),
        tools: c.tools, epoch: c.epoch, facts: c.facts ?? null, messages: c.messages, state_snapshot: c.state_snapshot ?? null,
        summary: c.summary ?? null, compactions: c.compactions });
    },
    async restoreConversation(snapshot: unknown, info: ModelLimits, allowed: string[], system?: string, inherit?: string) {
      const id = restore(snapshot, info, allowed, system, undefined, inherit);
      if (inherit == null) await refreshProviders(id,String(conversation(id).messages[0]?.content ?? ""));
      return id;
    },
    async restoreTask(snapshot: unknown, input: Omit<TaskInput, "prompt">) {
      const f = facts(input.facts);
      const id = restore(snapshot, input.model, toolNames(input.allowed_tools), agentRules(f), f, input.inherit);
      if (input.inherit == null) await refreshProviders(id,String(conversation(id).messages[0]?.content ?? ""));
      return id;
    },
    closeConversation(id: string) {
      if (typeof id !== "string") throw new Error("closeConversation requires a conversation id");
      if (!conversations.delete(id)) return;
      for (const key of refreshes.keys()) if (key.startsWith(id+":")) refreshes.delete(key);
    },
  };
}
