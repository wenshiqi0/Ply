// Exercises the real agent loop without HTTP, credentials, or a model process.
import { existsSync } from "node:fs";
async function modelRequest(request) {
  if (request.operation === "describe") return { contextWindow: 1_000_000, maxTokens: 16384 };
  const context = request.context;
  const task = JSON.parse(context.messages[0].content);
  const catalogLine = context.systemPrompt.split("Available skills (SKILL.md metadata only):\n")[1]?.split("\n")[0];
  const catalog = { skills: catalogLine ? JSON.parse(catalogLine) : [] };
  const names = catalog?.skills.map(s => s.name).sort() ?? [];
  if (JSON.stringify(names) !== JSON.stringify([...task.expected].sort())) throw new Error("wrong per-agent skill catalog: " + names);
  for (const skill of catalog?.skills ?? []) {
    if (!context.systemPrompt.includes(skill.path) || !context.systemPrompt.includes(skill.description)) throw new Error("skill not visible in model context");
  }
  if (context.systemPrompt.includes("BODY_ONLY_SECRET") || context.systemPrompt.includes("MIDDLE_ONLY_SECRET")) throw new Error("skill body was eagerly injected");
  for (const forbidden of task.forbidden ?? []) if (context.systemPrompt.includes(forbidden)) throw new Error("sibling catalog leaked into this agent");
  const results = context.messages.filter(m => m.role === "toolResult");
  const emit = (blocks, stopReason = "stop") => ({ role: "assistant", content: stopReason === "stop"
    ? [...blocks, { type: "text", text: "\n[[PLY_REPORT_COMPLETE]]" }] : blocks,
    stopReason, usage: { input: 20, output: 10, cacheRead: 80, cacheWrite: 0, totalTokens: 110 } });
  const final = text => emit([{ type: "text", text }]);
  const call = (name, args) => emit([{ type: "toolCall", id: "skill-test-" + results.length, name, arguments: args }], "toolUse");
  if (task.action === "pause" && !existsSync("continue-skills.flag")) throw new Error("skill recovery pause");
  if (task.action === "delegate") {
    if (!results.length) return call("agent", { prompt: JSON.stringify({ action: "child", expected: task.expected }) });
    const child = JSON.parse(results[0].content[0].text);
    if (child.text !== JSON.stringify(names)) throw new Error("child lost inherited skills");
    return final(child.text);
  }
  if (task.action === "child" && context.tools.some(t => t.name === "agent")) throw new Error("child can still delegate");
  if (context.tools.some(t => t.name === "skill")) throw new Error("skills must be context, not a model tool");
  if (task.action === "activate" || task.action === "deny" || task.action === "tamper" || task.action === "escalate") {
    const selected = catalog.skills.find(s => s.name === task.selected);
    const reads = results.filter(result => result.toolName === "read");
    if (!reads.length) return call("read", { path: selected.path, offset: 1, limit: 1 });
    if (task.action === "deny" || task.action === "tamper") {
      const expected = task.action === "deny" ? "no tool named read" : "invalid tool approval response";
      if (!reads[0].isError || !reads[0].content[0].text.includes(expected)) throw new Error("read grant or binding was bypassed");
      return final(task.action === "deny" ? "read grant retained" : "approval binding retained");
    }
    const last = JSON.parse(reads.at(-1).content[0].text);
    if (last.has_more) return call("read", { path: selected.path, offset: last.offset + last.lines, limit: 1 });
    const loaded = reads.map(result => JSON.parse(result.content[0].text).content).join("");
    if (!loaded.includes("BODY_ONLY_SECRET") || !loaded.includes("MIDDLE_ONLY_SECRET")) throw new Error("skill instructions were incomplete");
    if (task.action === "escalate") {
      const created = results.find(result => result.toolName === "create");
      if (!created) return call("create", { path: "must-not-exist.txt", content: "forbidden" });
      if (!created.isError || !created.content[0].text.includes("no tool named create")) throw new Error("skill granted a new tool");
      return final("skill cannot grant tools");
    }
    return final("skill fully read: " + selected.name);
  }
  return final(JSON.stringify(names));
}
