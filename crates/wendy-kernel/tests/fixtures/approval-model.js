// Local model boundary for approval tests; used with the real agent loop.
const fixtureMarkdownReply = [
  "## 项目结论", "",
  "这段包含 **加粗**、*斜体*、`inline()` 和 ~~旧方案~~。", "",
  "### 执行步骤", "",
  "- 检查模块", "  - 保留嵌套层级", "- [x] 完成解析", "",
  "1. 先分析", "2. 再修改", "",
  "> 注意边界：代码内容保持原样。", "",
  "```ts", "const text = '**代码字面量**';", "  // 保留缩进", "```", "",
  "| 模块 | 状态 |", "| :--- | ---: |", "| parser | ready |", "| 终端 | 完成 |", "",
  "查看 [文档](https://example.com/wendy)。",
].join("\n");
function fixtureReply(content, stopReason) {
  return { role: "assistant", content, stopReason,
    usage: { input: 20, output: 10, cacheRead: 80, cacheWrite: 0, reasoning: 0, totalTokens: 110 } };
}
async function modelRequest(req) {
  if (req.operation === "describe") return { contextWindow: 32768, maxTokens: 1024 };
  const prompt = req.context.messages.find(m => m.role === "user").content;
  const names = req.context.tools.map(t => t.name).sort();
  await fiber.record({ type: "fixture_offered_tools", names, prompt });
  const results = req.context.messages.filter(m => m.role === "toolResult");
  if (results.length) {
    const summary = results.map(r => {
      const text = r.content.map(c => c.text ?? "").join("");
      let output; try { output = JSON.parse(text); } catch { output = text; }
      return { tool: r.toolName, output: r.toolName === "bash" ? { stdout: output.stdout, stderr: output.stderr, exit_code: output.exit_code }
        : r.toolName === "agent" ? output.text : output };
    });
    const text = results.some(r => r.isError) ? "tool-rejected" : "agent-done " + JSON.stringify(summary);
    return fixtureReply([{ type: "text", text: text + "\n[[WENDY_REPORT_COMPLETE]]" }], "stop");
  }
  let calls;
  if (prompt.startsWith("/bash ")) calls = [{ name: "bash", arguments: { command: prompt.slice(6) } }];
  else if (prompt.startsWith("call:")) {
    const call = JSON.parse(prompt.slice(5)); calls = [{ name: call.tool, arguments: call.input }];
  } else if (prompt === "parallel") calls = ["one", "two"].map(name => ({ name: "bash", arguments: { command: "echo " + name + " > " + name } }));
  else if (prompt === "double-read") calls = ["first.txt", "second.txt"].map(path => ({ name: "read", arguments: { path } }));
  else if (prompt === "delegate") calls = [{ name: "agent", arguments: { prompt: "child" } }];
  else if (prompt === "delegate-many") calls = [0,1,2].map(() => ({ name: "agent", arguments: { prompt: "child" } }));
  else if (prompt === "child") calls = [{ name: "read", arguments: { path: "secret.txt" } }];
  else if (prompt === "ansi") calls = [{ name: "bash", arguments: { command: "echo '\x1b]52;c;injected\x07\u202e' > ansi" } }];
  else if (prompt === "cancel") {
    await new Promise(resolve => setTimeout(resolve, 5000));
    calls = [{ name: "bash", arguments: { command: "echo late > late" } }];
  } else if (prompt === "markdown-demo") {
    return fixtureReply([{ type: "text", text: fixtureMarkdownReply + "\n[[WENDY_REPORT_COMPLETE]]" }], "stop");
  } else if (prompt === "slow-chat") {
    await new Promise(resolve => setTimeout(resolve, 1000));
    return fixtureReply([{ type: "text", text: "agent-chat: slow-chat\n[[WENDY_REPORT_COMPLETE]]" }], "stop");
  } else return fixtureReply([{ type: "text", text: "agent-chat: " + prompt + "\n[[WENDY_REPORT_COMPLETE]]" }], "stop");
  return fixtureReply(calls.map((call, i) => ({ type: "toolCall", id: "call-" + i, ...call })), "toolUse");
}
