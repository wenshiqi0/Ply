// Model boundary for the agent-loop tests. It runs in the same JSC realm
// and never opens a socket or starts a process.
import { existsSync as canRecoverSummary } from "node:fs";
const summaryFailedOnce = new Set();

async function modelRequest(req) {
  if (req.operation === "describe") return { contextWindow: 1_000_000, maxTokens: 131_072 };
  const ctx = req.context;
  let task = ctx.messages[0].content;
  const results = ctx.messages.filter(message => message.role === "toolResult");
  const usage = (input, output, cacheRead, cacheWrite = 0, reasoning = 0) => ({
    input, output, cacheRead, cacheWrite, reasoning, totalTokens: input + output + cacheRead + cacheWrite,
  });
  const emit = (content, tokens, stopReason = "stop", errorMessage) => {
    const rejectedDraft = ["report repaired", "report length"].includes(task) && ctx.messages.filter(message => message.role === "user").length === 1;
    const final = req.purpose !== "compaction" && stopReason === "stop" && !rejectedDraft && !content.some(block => block.type === "toolCall");
    return {
      role: "assistant", content: final ? [...content, { type: "text", text: "\n[[WENDY_REPORT_COMPLETE]]" }] : content,
      stopReason, responseId: "fake-" + results.length,
      ...(tokens === undefined ? {} : { usage: tokens }), ...(errorMessage ? { errorMessage } : {}),
    };
  };
  const text = value => [{ type: "text", text: value }];
  const call = (id, name, args) => ({ type: "toolCall", id, name, arguments: args });
  if (req.purpose === "compaction") {
    if (ctx.tools.length) throw new Error("summarization must not receive tools");
    task = JSON.parse(ctx.messages[0].content).original_task;
    if (task === "compact transient" && !summaryFailedOnce.has(task)) {
      summaryFailedOnce.add(task);
      return emit([], usage(100, 0, 0), "error", "temporary summary failure");
    }
    if (task === "compact recover" && !canRecoverSummary("allow-summary.flag")) return emit([], usage(100, 0, 0), "error", "summary service offline");
    if (task === "compact soft") return emit([], usage(100, 0, 0), "error", "summary service offline");
    if (task === "compact fail") return emit([], usage(100, 0, 0), "error", "summary unavailable");
    if (task === "compact truncated") return emit(text("incomplete memory"), usage(100, 20, 0), "length");
    if (task === "compact tool") return emit([call("forbidden", "read", { path: "compact.txt" })], usage(100, 20, 0), "toolUse");
    if (task === "compact empty") return emit([], usage(100, 20, 0));
    if (task === "compact missing") return emit(text("Memory for " + task + ": continue with the retained evidence."));
    return emit(text("Memory for " + task + ": evidence preserved; continue the original task."), usage(100, 20, 0));
  }
  if (["compact soft", "compact transient", "compact recover", "compact root", "compact A", "compact B", "compact fail", "compact truncated", "compact tool", "compact empty", "compact missing"].includes(task)) {
    const tokens = usage(Math.max(1, Math.floor(JSON.stringify(ctx).length / 4)), 10, 0);
    const pending = new Set();
    for (const message of ctx.messages) {
      if (message.role === "assistant") {
        for (const block of message.content) if (block.type === "toolCall") pending.add(block.id);
      } else if (message.role === "toolResult") {
        if (!pending.delete(message.toolCallId)) throw new Error("orphan tool result after compaction");
      }
    }
    if (pending.size) throw new Error("pending tool calls sent to model");
    if (task === "compact root") {
      if (!results.length) return emit([
        call("A", "agent", { prompt: "compact A" }), call("B", "agent", { prompt: "compact B" }),
      ], tokens, "toolUse");
      if (results.some(result => result.isError)) throw new Error("a compacting child failed");
      const children = results.map(result => JSON.parse(result.content[0].text));
      if (JSON.stringify(children.map(child => child.text)) !== JSON.stringify(["done compact A", "done compact B"])) {
        throw new Error("compacting children crossed conversations");
      }
      return emit(text("compacting children completed"), tokens);
    }
    const other = task === "compact A" ? "compact B" : "compact A";
    if (ctx.messages.some(message => JSON.stringify(message.content).includes(other))) throw new Error("another agent memory leaked into this context");
    const ids = results.filter(result => result.toolName === "read").map(result => Number(result.toolCallId.split("-").at(-1)));
    const step = Math.max(-1, ...ids) + 1;
    const limit = task === "compact soft" ? 3 : ["compact fail", "compact truncated", "compact tool", "compact empty"].includes(task)
      || task === "compact recover" && !canRecoverSummary("allow-summary.flag") ? 20 : 8;
    return step < limit
      ? emit([call("read-" + step, "read", { path: "compact.txt", limit: 200 })], tokens, "toolUse")
      : emit(text("done " + task), tokens);
  }
  if (task === "bash hook") {
    if (!results.length) return emit([
      call("allowed", "bash", { command: "echo $SHELL" }),
      call("blocked", "bash", { command: "echo __deny_demo__ > blocked-model-marker" }),
    ], usage(10, 5, 90), "toolUse");
    const allowed = results.find(result => result.toolCallId === "allowed");
    const blocked = results.find(result => result.toolCallId === "blocked");
    if (allowed.isError || JSON.parse(allowed.content[0].text).stdout !== "wendy-sh\n" || !blocked.isError) throw new Error("bash backend or before hook failed");
    return emit(text("wendy sh hook works"), usage(10, 5, 90));
  }
  if (task === "report root") return !results.length
    ? emit([call("report-child", "agent", { prompt: "report repaired", allowed_tools: ["read"], max_delegation_depth: 0 })], usage(10, 5, 90), "toolUse")
    : emit(text(JSON.parse(results[0].content[0].text).text), usage(10, 5, 90));
  if (task === "report repaired") return ctx.messages.filter(message => message.role === "user").length === 1
    ? emit(text("## 架构判断\nDraft.\n</｜｜DSML｜｜ parameter>"), usage(10, 5, 90))
    : emit(text("## 架构判断\nComplete.\n## 发现\nNo confirmed defects.\n## 验证边界\nOffline fixture only."), usage(10, 5, 90));
  if (task === "report length") return ctx.messages.filter(message => message.role === "user").length === 1
    ? emit(text("## Findings\n```text\ntruncated"), usage(10, 5, 90), "length")
    : emit(text("## Findings\nThe complete report fits."), usage(10, 5, 90));
  if (task === "quota leaf") return emit(text("leaf"), usage(10, 5, 90));
  if (task === "quota resume") {
    if (!results.length) return emit([call("first", "agent", { prompt: "quota leaf" }), call("second", "agent", { prompt: "quota leaf" })], usage(10, 5, 90), "toolUse");
    if (!canRecoverSummary("quota-allow.flag")) throw new Error("pause after spending both slots");
    if (!results.some(result => result.toolCallId === "replacement")) return emit([call("replacement", "agent", { prompt: "quota leaf" })], usage(10, 5, 90), "toolUse");
    const rejected = results.find(result => result.toolCallId === "replacement");
    return emit(text(rejected.isError ? "quota preserved" : "quota reset incorrectly"), usage(10, 5, 90));
  }
  if (task === "policy root") return !results.length
    ? emit([call("leaf", "agent", { prompt: "policy leaf" })], usage(10, 5, 0), "toolUse")
    : emit(text(JSON.parse(results[0].content[0].text).text), usage(10, 5, 0));
  if (task === "policy leaf") return !results.length
    ? emit([call("forbidden-child", "agent", { prompt: "grandchild", allowed_tools: ["create"], max_delegation_depth: 99 })], usage(10, 5, 0), "toolUse")
    : emit(text(JSON.stringify({ tools: ctx.tools.map(tool => tool.name), denied: results[0].isError })), usage(10, 5, 0));
  if (task === "policy escalation") return !results.length
    ? emit([call("escalate", "agent", { prompt: "grandchild", allowed_tools: ["create"], max_delegation_depth: 99 })], usage(10, 5, 0), "toolUse")
    : emit(text(JSON.stringify({ denied: results[0].isError, reason: results[0].content[0].text })), usage(10, 5, 0));
  if (task === "task truncated") return emit([...text("partial answer"), call("partial", "read", { path: "made.txt" })], usage(25, 10, 75), "length");
  if (task === "transport-fail") throw new Error("transport unavailable");
  if (task === "fail") return emit([], usage(5, 0, 0), "error", "rate limited");
  if (task === "missing-usage") return emit(text("no usage"));
  if (task === "zero-usage") return emit(text("placeholder usage"), usage(0, 0, 0));
  if (task === "delegate-missing") return !results.length
    ? emit([call("missing", "agent", { prompt: "missing-usage" })], usage(10, 5, 90), "toolUse")
    : emit(text("done"), usage(10, 5, 90));
  if (["stress root", "stress A", "stress B"].includes(task)) {
    // Exercise the host-completion/microtask path while siblings run.
    await new Promise(resolve => setTimeout(resolve, 100));
    const tokens = usage(10, 5, 90, 0, 2);
    if (results.some(result => result.isError)) throw new Error("a stress tool call failed");
    if (task === "stress root") {
      if (!results.length) return emit([
        call("A", "agent", { prompt: "stress A" }), call("B", "agent", { prompt: "stress B" }),
      ], tokens, "toolUse");
      const children = results.map(result => JSON.parse(result.content[0].text));
      if (JSON.stringify(children.map(child => child.text)) !== JSON.stringify(["done stress A", "done stress B"])) {
        throw new Error("child conversations crossed");
      }
      return emit(text("both children completed"), tokens);
    }
    const reads = results.filter(result => result.toolName === "read").length;
    if (reads < 24) {
      const calls = [call("read-" + reads, "read", { path: "large.txt", limit: 200 })];
      if (task === "stress A" && !results.length) calls.push(call("nested", "agent", { prompt: "grandchild" }));
      return emit(calls, tokens, "toolUse");
    }
    return emit(text("done " + task), tokens);
  }
  if (["delegate", "child branch", "grandchild", "child fail"].includes(task)) {
    const tokens = usage(10, 5, 90, 0, 2);
    if (task === "child fail") return emit([], tokens, "error", "child failed after reporting usage");
    if (task === "delegate" && !results.length) return emit([
      call("child1", "agent", { prompt: "child branch" }), call("child2", "agent", { prompt: "child fail" }),
    ], tokens, "toolUse");
    if (task === "child branch" && !results.length) return emit([call("grandchild", "agent", { prompt: "grandchild" })], tokens, "toolUse");
    return emit(text("done " + task), tokens);
  }
  if (!results.length) return emit([
    ...text("working"), call("c1", "create", { path: "made.txt", content: "by agent" }), call("c2", "nope", {}),
  ], usage(100, 20, 300, 100, 5), "toolUse");
  const seen = results.map(result => [result.toolName, result.isError, result.content[0].text.slice(0, 30)]);
  const spacedJSON = value => Array.isArray(value) ? "[" + value.map(spacedJSON).join(", ") + "]" : JSON.stringify(value);
  return emit(text("tools=" + ctx.tools.map(tool => tool.name).sort().join(",") + " key=" + req.profile.apiKeyEnv + " results=" + spacedJSON(seen)), usage(50, 30, 950, 0, 7));
}
