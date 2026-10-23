import { execFile } from "node:child_process";

// Only the model connection comes from pi: each turn runs a short node
// process that loads pi-ai (from the installed pi, or WENDY_PI_AI), reads
// { model, context } on stdin and writes the model's reply. The
// conversation itself is kept here, for one task, and then dropped.
const MODEL_CALL = `
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
const chunks = [];
for await (const chunk of process.stdin) chunks.push(chunk);
const request = JSON.parse(Buffer.concat(chunks).toString("utf8"));
const installed = join(dirname(process.execPath), "../lib/node_modules/@earendil-works");
const root = [process.env.WENDY_PI_AI, join(installed, "pi-ai"), join(installed, "pi-coding-agent/node_modules/@earendil-works/pi-ai")]
  .find((p) => p && existsSync(join(p, "dist/providers/all.js")));
if (!root) {
  console.error("pi-ai not found: install @earendil-works/pi-coding-agent, or set WENDY_PI_AI to a pi-ai package");
  process.exit(2);
}
const { builtinModels } = await import(pathToFileURL(join(root, "dist/providers/all.js")).href);
const models = builtinModels();
const slash = request.model.indexOf("/");
const model = slash > 0 ? models.getModel(request.model.slice(0, slash), request.model.slice(slash + 1)) : undefined;
if (!model) {
  console.error("unknown model " + request.model + " (expected provider/model-id)");
  process.exit(2);
}
process.stdout.write(JSON.stringify(await models.complete(model, request.context)));
`;

type Block = { type: string; text?: string; id?: string; name?: string; arguments?: Record<string, unknown> };
type Message = { role: string; content: unknown; stopReason?: string; errorMessage?: string; timestamp: number };

/**
 * Work on a task with a language model, using the other plugins as its
 * tools. pi's model layer (pi-ai) connects to the model; the conversation
 * lives only for this task.
 */
export async function handle(input: {
  /** The task, in plain words. */
  prompt: string;
  /** provider/model-id, e.g. "deepseek/deepseek-v4-flash" (the default). */
  model?: string;
  /** Give up after this many model turns (default 20). */
  max_turns?: number;
}): Promise<{ text: string; model: string; turns: number; tool_calls: number }> {
  if (typeof input?.prompt !== "string" || input.prompt.trim() === "") throw new Error("'prompt' must be a non-empty string");
  const model = input.model ?? "deepseek/deepseek-v4-flash";
  const maxTurns = input.max_turns ?? 20;
  if (!Number.isInteger(maxTurns) || maxTurns < 1) throw new Error("'max_turns' must be a positive integer");

  const names = Object.keys(tools).filter((name) => name !== "agent");
  const context = {
    systemPrompt:
      `You are an agent working in the directory ${process.cwd()}. ` +
      "Use the tools to look at and change things there; relative paths are relative to that directory. " +
      "When the task is done, answer with a short summary of what you did.",
    messages: [{ role: "user", content: input.prompt, timestamp: Date.now() }] as Message[],
    tools: names.map((name) => ({ name, description: tools[name].description, parameters: tools[name].parameters })),
  };

  let toolCalls = 0;
  for (let turn = 1; turn <= maxTurns; turn++) {
    const reply = await complete(model, context);
    if (reply.stopReason === "error" || reply.stopReason === "aborted") {
      throw new Error(`${model}: ${reply.errorMessage ?? reply.stopReason}`);
    }
    context.messages.push(reply);
    const blocks = (reply.content ?? []) as Block[];
    const calls = blocks.filter((b) => b.type === "toolCall");
    if (calls.length === 0) {
      const text = blocks.filter((b) => b.type === "text").map((b) => b.text).join("");
      return { text, model, turns: turn, tool_calls: toolCalls };
    }
    toolCalls += calls.length;
    // Each call is a child fiber of this one; independent calls run together.
    context.messages.push(...(await Promise.all(calls.map((call) => runTool(call, names)))));
  }
  throw new Error(`no answer after ${maxTurns} model turns`);
}

async function runTool(call: Block, names: string[]): Promise<Message> {
  const name = call.name ?? "";
  let text: string;
  let isError = false;
  try {
    if (!names.includes(name)) throw new Error(`no tool named ${name}`);
    const tool = tools[name];
    const output = await tool(tool.wrapsInput ? call.arguments?.input : (call.arguments ?? {}));
    text = typeof output === "string" ? output : JSON.stringify(output);
  } catch (e) {
    isError = true;
    text = e instanceof Error ? e.message : String(e);
  }
  return { role: "toolResult", toolCallId: call.id, toolName: name, content: [{ type: "text", text }], isError, timestamp: Date.now() } as Message;
}

function complete(model: string, context: unknown): Promise<Message> {
  const options = { input: JSON.stringify({ model, context }), timeout: 600_000, maxBuffer: 16 << 20 };
  return new Promise((resolve, reject) => {
    execFile("node", ["--input-type=module", "-e", MODEL_CALL], options, (err: any, stdout: string, stderr: string) => {
      if (err) return reject(new Error(`model call failed: ${(stderr || err.message).trim()}`));
      try {
        resolve(JSON.parse(stdout));
      } catch {
        reject(new Error(`model call returned something that is not JSON: ${stdout.slice(0, 200)}`));
      }
    });
  });
}
