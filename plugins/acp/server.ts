import * as events from "ply:events";
import { agent, approval } from "ply:plugins";
import type { HostImage } from "ply:plugins";
import type { Active, Catalog, Decision, ObjectValue, Options, Permission } from "./types";
import { Sessions } from "./sessions";
import { object, ProtocolError, text, Transport } from "./transport";

export async function prepareServer(options: Options): Promise<() => Promise<void>> {
  let catalog: Catalog = [];
  let sessions: Sessions;
  let running = false;
  let initialized = false;
  let active: Active | undefined;
  let creating = false;
  const claimed = new Set<string>();
  let transport: Transport;
  const update = (value: ObjectValue) => {
    if (active) transport.notify("session/update", { sessionId: active.session.id, update: value });
  };
  function cancel(id?: unknown): void {
    if (active && (id === undefined || active.session.id === id)) {
      active.cancelled = true; active.call?.cancel(); transport.cancelRequests();
    }
  }
  async function permission(event: events.Event<Permission>): Promise<Decision> {
    const turn = active;
    const request = event.data;
    if (!turn || turn.cancelled) return { request_id: request.id, decision: "pending", reason: "no active ACP prompt" };
    const tool = [...turn.tools].find(([id, value]) => !claimed.has(id) && value.name === request.tool);
    const toolId = tool?.[0] ?? request.id;
    claimed.add(toolId);
    try {
      const result = object(await transport.request("session/request_permission", {
        sessionId: turn.session.id,
        toolCall: { toolCallId: toolId, title: request.tool, status: "pending", rawInput: "command" in request ? { command: request.command, cwd: request.cwd } : request.input },
        options: [ { optionId: "allow", name: "Allow once", kind: "allow_once" }, { optionId: "reject", name: "Reject once", kind: "reject_once" } ],
      }));
      const outcome = object(result.outcome);
      if (outcome.outcome === "cancelled") cancel(turn.session.id);
      const accepted = !turn.cancelled && outcome.outcome === "selected" && outcome.optionId === "allow";
      if (!turn.cancelled && !(outcome.outcome === "selected" && ["allow", "reject"].includes(String(outcome.optionId)))) throw new Error("invalid permission outcome");
      return { request_id: request.id, decision: accepted ? "approved" : "rejected", reason: accepted ? "ACP client allowed this operation" : "ACP client rejected or cancelled this operation" };
    } catch { return { request_id: request.id, decision: "rejected", reason: "ACP permission unavailable" }; }
    finally { claimed.delete(toolId); }
  }
  async function prompt(input: ObjectValue): Promise<ObjectValue> {
    const session = sessions.get(input.sessionId);
    if (active || creating) throw new ProtocolError(-32000, "another prompt is running");
    if (!Array.isArray(input.prompt) || !input.prompt.length) throw new ProtocolError(-32602, "prompt must contain content blocks");
    const texts: string[] = [], images: HostImage[] = [];
    const selected = catalog.find(row => row.id === session.model)!;
    for (const raw of input.prompt) {
      const block = object(raw);
      if (block.type === "text") texts.push(text(block.text));
      else if (block.type === "image" && selected.input.includes("image")) images.push({ type: "image", data: text(block.data), mimeType: text(block.mimeType) });
      else throw new ProtocolError(-32602, "unsupported prompt content");
    }
    const turn: Active = { session, cancelled: false, tools: new Map() };
    active = turn;
    try {
      await approval.useDefault("ask");
      if (turn.cancelled) return { stopReason: "cancelled" };
      const call = agent.hostChat({ prompt: texts.join("\n") || "Describe the supplied images.", images,
        model: session.model, effort: session.effort, snapshot: session.snapshot });
      turn.call = call;
      const output = await call;
      if (turn.cancelled) return { stopReason: "cancelled" };
      if (output.result.status === "approval_required" || output.snapshot === undefined) throw new Error("agent did not complete the turn");
      session.snapshot = output.snapshot;
      session.history.push(...input.prompt.map(content => ({ role: "user" as const, content })),
        { role: "agent", content: { type: "text", text: output.result.text } });
      await sessions.save(session);
      update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: output.result.text } });
      return { stopReason: "end_turn" };
    } catch (error) {
      if (turn.cancelled) { await agent.hostIdle(); return { stopReason: "cancelled" }; }
      throw error;
    } finally {
      for (const id of turn.tools.keys()) update({ sessionUpdate: "tool_call_update", toolCallId: id, status: "failed", content: [{ type: "content", content: { type: "text", text: turn.cancelled ? "cancelled" : "turn ended" } }] });
      active = undefined; transport.cancelRequests();
    }
  }
  async function dispatch(method: string, input: ObjectValue): Promise<unknown> {
    if (method === "initialize") {
      if (initialized || !Number.isSafeInteger(input.protocolVersion) || Number(input.protocolVersion) < 1) throw new ProtocolError(-32602, "ACP protocol version 1 is required; initialize once");
      initialized = true;
      return { protocolVersion: 1, agentInfo: { name: "ply", version: process.versions.ply }, authMethods: [],
        agentCapabilities: { loadSession: true, promptCapabilities: { image: catalog.some(row => row.input.includes("image")) }, mcpCapabilities: { http: false, sse: false } },
        _meta: { ply: { models: catalog.map(({ id, name, input, efforts, defaultEffort }) => ({ id, name, input, efforts, defaultEffort })) } } };
    }
    if (!initialized) throw new ProtocolError(-32000, "initialize the connection first");
    if (method === "session/cancel") { cancel(input.sessionId); return; }
    if (method === "session/new" || method === "session/load") {
      if (active || creating) throw new ProtocolError(-32000, "another session operation is running");
      creating = true;
      try {
        const session = await (method === "session/new" ? sessions.create(input) : sessions.load(input));
        if (method === "session/load") {
          for (const entry of session.history) transport.notify("session/update", { sessionId: session.id,
            update: { sessionUpdate: entry.role === "user" ? "user_message_chunk" : "agent_message_chunk", content: entry.content } });
        }
        return { sessionId: session.id, configOptions: sessions.config(session) };
      } finally { creating = false; }
    }
    if (method === "session/set_config_option") {
      if (active || creating) throw new ProtocolError(-32000, "another session operation is running");
      creating = true;
      try {
        const session = sessions.get(input.sessionId);
        await sessions.select(session, input);
        const configOptions = sessions.config(session);
        transport.notify("session/update", { sessionId: session.id, update: { sessionUpdate: "config_option_update", configOptions } });
        return { configOptions };
      } finally { creating = false; }
    }
    if (method === "session/prompt") return prompt(input);
    throw new ProtocolError(-32601, "method not found: " + method);
  }
  await events.on("approval.request", permission);
  await events.on("approval.toolRequest", permission);
  await events.on("agent.tool_call", event => {
      if (!active || active.cancelled) return;
      const data = object(event.data), id = text(data.id), name = text(data.name);
      active.tools.set(id, { name, input: data.input });
      update({ sessionUpdate: "tool_call", toolCallId: id, title: name, status: "in_progress", rawInput: data.input });
  });
  await events.on("agent.tool_result", event => {
      if (!active || active.cancelled) return;
      const data = object(event.data), id = text(data.id);
      if (!active.tools.delete(id)) return;
      update({ sessionUpdate: "tool_call_update", toolCallId: id, status: data.isError ? "failed" : "completed", rawOutput: data.output,
        content: [{ type: "content", content: { type: "text", text: typeof data.output === "string" ? data.output : JSON.stringify(data.output) } }] });
  });
  return async () => {
    if (running) throw new Error("ACP server already running");
    running = true;
    try {
      catalog = await agent.modelCatalog();
      sessions = new Sessions(options.stateDirectory ?? text(process.env.PLY_DATA_DIRECTORY), catalog);
      initialized = false;
      transport = new Transport(dispatch);
      await transport.run(() => cancel());
    } finally { cancel(); running = false; }
  };
}
