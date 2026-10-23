import { execFile } from "node:child_process";

// The agent loop. The context plugin keeps the conversation and says which
// model to use (a model integration such as deepseek told it); the model
// call itself happens here: a short node process loads pi's model layer
// (pi-ai, from the installed pi or WENDY_PI_AI), reads { profile, context }
// on stdin and writes the model's reply.
const MODEL_CALL = `
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
const chunks = [];
for await (const chunk of process.stdin) chunks.push(chunk);
const { profile, context } = JSON.parse(Buffer.concat(chunks).toString("utf8"));
const installed = join(dirname(process.execPath), "../lib/node_modules/@earendil-works");
const root = [process.env.WENDY_PI_AI, join(installed, "pi-ai"), join(installed, "pi-coding-agent/node_modules/@earendil-works/pi-ai")]
  .find((p) => p && existsSync(join(p, "dist/providers/all.js")));
if (!root) {
  console.error("pi-ai not found: install @earendil-works/pi-coding-agent, or set WENDY_PI_AI to a pi-ai package");
  process.exit(2);
}
const { builtinModels } = await import(pathToFileURL(join(root, "dist/providers/all.js")).href);
const models = builtinModels();
const model = models.getModel(profile.provider, profile.model);
if (!model) {
  console.error("unknown model " + profile.provider + "/" + profile.model);
  process.exit(2);
}
const options = profile.apiKeyEnv ? { apiKey: process.env[profile.apiKeyEnv] } : {};
process.stdout.write(JSON.stringify(await models.complete(model, context, options)));
`;

type Block = { type: string; text?: string; id?: string; name?: string; arguments?: Record<string, unknown> };
type Message = {
  role: string;
  content: unknown;
  stopReason?: string;
  errorMessage?: string;
  responseId?: string;
  usage?: unknown;
  timestamp: number;
};
type Context = { systemPrompt: string; messages: Message[]; tools: { name: string }[] };

type TokenUsage = {
  input_tokens: number;
  uncached_input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  reasoning_tokens: number | null;
  total_tokens: number;
  cache_hit_rate: number | null;
};

type UsageCounts = {
  calls: number;
  reported_calls: number;
  missing_calls: number;
  input_tokens: number;
  uncached_input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  reasoning_tokens: number | null;
  total_tokens: number;
};

type UsageSummary = {
  calls: number;
  reported_calls: number;
  missing_calls: number;
  complete: boolean;
  input_tokens: number;
  uncached_input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  reasoning_tokens: number | null;
  total_tokens: number;
  cache_hit_rate: number | null;
};

type RunUsage = { own: UsageCounts; total: UsageCounts; parent?: RunUsage };
// Calls of this plugin share a realm. Link active agents by the kernel's
// ancestry, so even a child that fails contributes usage to its ancestors.
const activeUsage = new Map<string, RunUsage>();

/**
 * Run a task in a new agent with its own conversation and the tools offered
 * in the context. Include the relevant context and constraints in the prompt.
 * The agent can delegate further subtasks with this same tool.
 */
export async function onHandle(input: {
  /** A self-contained task, including the context and constraints this agent needs. */
  prompt: string;
  /** Give up after this many model turns (default 20). */
  max_turns?: number;
}): Promise<{ text: string; model: string; turns: number; tool_calls: number; usage: UsageSummary; total_usage: UsageSummary }> {
  if (typeof input?.prompt !== "string" || input.prompt.trim() === "") throw new Error("'prompt' must be a non-empty string");
  const maxTurns = input.max_turns ?? 20;
  if (!Number.isInteger(maxTurns) || maxTurns < 1) throw new Error("'max_turns' must be a positive integer");

  await tools.context.loaded;
  const profile = await ctx.currentModel();
  if (!profile) throw new Error("no model in the context: load a model integration such as deepseek");
  const model = `${profile.provider}/${profile.model}`;
  const system =
    `You are an agent working in the directory ${process.cwd()}. ` +
    "Use the tools to look at and change things there; relative paths are relative to that directory. " +
    "Prefer the read tool for reading files when it is available. " +
    "Use the agent tool to delegate subtasks when useful. Each child has its own conversation, so include the relevant context and constraints in its prompt. " +
    "When the task is done, answer with a short summary of what you did.";
  const current = await fiber.info();
  const id = await ctx.openConversation(system);
  const run = startUsage(current.id, current.ancestors);
  let completed = false;
  try {
    let context: Context = await ctx.appendMessages(id, [{ role: "user", content: input.prompt, timestamp: Date.now() }]);
    let toolCalls = 0;
    for (let turn = 1; turn <= maxTurns; turn++) {
      const reply: Message = { ...(await completeTurn(profile, context, model, turn, run)), timestamp: Date.now() };
      if (reply.stopReason === "error" || reply.stopReason === "aborted") {
        throw new Error(`${model}: ${reply.errorMessage ?? reply.stopReason}`);
      }
      context = await ctx.appendMessages(id, [reply]);
      const blocks = (reply.content ?? []) as Block[];
      const calls = blocks.filter((b) => b.type === "toolCall");
      if (calls.length === 0) {
        const text = blocks.filter((b) => b.type === "text").map((b) => b.text).join("");
        completed = true;
        return { text, model, turns: turn, tool_calls: toolCalls, usage: usageSummary(run.own), total_usage: usageSummary(run.total) };
      }
      toolCalls += calls.length;
      const offered = context.tools.map((t) => t.name);
      // Each call is a child fiber of this one; independent calls run together.
      context = await ctx.appendMessages(id, await Promise.all(calls.map((call) => runTool(call, offered))));
    }
    throw new Error(`no answer after ${maxTurns} model turns`);
  } finally {
    try {
      await fiber.record({
        type: "agent_usage",
        model,
        status: completed ? "completed" : "failed",
        usage: usageSummary(run.own),
        total_usage: usageSummary(run.total),
      });
    } finally {
      activeUsage.delete(current.id);
      await ctx.closeConversation(id);
    }
  }
}

function emptyUsage(): UsageCounts {
  return {
    calls: 0, reported_calls: 0, missing_calls: 0,
    input_tokens: 0, uncached_input_tokens: 0, output_tokens: 0,
    cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0, total_tokens: 0,
  };
}

function startUsage(id: string, ancestors: string[]): RunUsage {
  let parent: RunUsage | undefined;
  for (const ancestor of ancestors) {
    parent = activeUsage.get(ancestor);
    if (parent) break;
  }
  const run: RunUsage = { own: emptyUsage(), total: emptyUsage(), parent };
  activeUsage.set(id, run);
  return run;
}

function isTokenCount(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function tokenUsage(raw: any): TokenUsage | null {
  if (!raw || ![raw.input, raw.output, raw.cacheRead, raw.cacheWrite].every(isTokenCount)) return null;
  // pi-ai's input excludes cache reads/writes; output includes reasoning.
  const input = raw.input + raw.cacheRead + raw.cacheWrite;
  const total = input + raw.output;
  // The SDK also uses all-zero usage as a placeholder before usage arrives.
  if (!isTokenCount(input) || !isTokenCount(total) || total === 0) return null;
  return {
    input_tokens: input,
    uncached_input_tokens: raw.input,
    output_tokens: raw.output,
    cache_read_tokens: raw.cacheRead,
    cache_write_tokens: raw.cacheWrite,
    reasoning_tokens: isTokenCount(raw.reasoning) && raw.reasoning <= raw.output ? raw.reasoning : null,
    total_tokens: total,
    cache_hit_rate: input > 0 ? raw.cacheRead / input : null,
  };
}

function addUsage(counts: UsageCounts, usage: TokenUsage | null) {
  counts.calls++;
  if (!usage) {
    counts.missing_calls++;
    counts.reasoning_tokens = null;
    return;
  }
  counts.reported_calls++;
  counts.input_tokens += usage.input_tokens;
  counts.uncached_input_tokens += usage.uncached_input_tokens;
  counts.output_tokens += usage.output_tokens;
  counts.cache_read_tokens += usage.cache_read_tokens;
  counts.cache_write_tokens += usage.cache_write_tokens;
  counts.total_tokens += usage.total_tokens;
  counts.reasoning_tokens = counts.reasoning_tokens === null || usage.reasoning_tokens === null
    ? null : counts.reasoning_tokens + usage.reasoning_tokens;
}

function rememberUsage(run: RunUsage, usage: TokenUsage | null) {
  addUsage(run.own, usage);
  for (let ancestor: RunUsage | undefined = run; ancestor; ancestor = ancestor.parent) addUsage(ancestor.total, usage);
}

function usageSummary(counts: UsageCounts): UsageSummary {
  const complete = counts.missing_calls === 0;
  return {
    ...counts,
    complete,
    // Weight by input tokens, not by the number of calls. A partial run
    // cannot claim a hit rate for all of its calls.
    cache_hit_rate: complete && counts.input_tokens > 0 ? counts.cache_read_tokens / counts.input_tokens : null,
  };
}

async function completeTurn(profile: unknown, context: Context, model: string, turn: number, run: RunUsage): Promise<Message> {
  const started = Date.now();
  let reply: Message;
  try {
    reply = await complete(profile, context);
  } catch (error) {
    rememberUsage(run, null);
    await fiber.record({
      type: "model_call", turn, model, response_id: null, stop_reason: "error",
      duration_ms: Date.now() - started, usage: null,
      error: error instanceof Error ? error.message : String(error),
    });
    throw error;
  }
  const usage = tokenUsage(reply.usage);
  rememberUsage(run, usage);
  await fiber.record({
    type: "model_call", turn, model, response_id: reply.responseId ?? null,
    stop_reason: reply.stopReason ?? null, duration_ms: Date.now() - started, usage,
    error: reply.errorMessage ?? null,
  });
  return reply;
}

async function runTool(call: Block, offered: string[]): Promise<Message> {
  const name = call.name ?? "";
  let text: string;
  let isError = false;
  try {
    if (!offered.includes(name)) throw new Error(`no tool named ${name}`);
    const tool = tools[name];
    const output = await tool(tool.wrapsInput ? call.arguments?.input : (call.arguments ?? {}));
    text = typeof output === "string" ? output : JSON.stringify(output);
  } catch (e) {
    isError = true;
    text = e instanceof Error ? e.message : String(e);
  }
  return { role: "toolResult", toolCallId: call.id, toolName: name, content: [{ type: "text", text }], isError, timestamp: Date.now() } as Message;
}

function complete(profile: unknown, context: Context): Promise<Message> {
  const options = { input: JSON.stringify({ profile, context }), timeout: 600_000, maxBuffer: 16 << 20 };
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

/** Offer this plugin to the model so agents can start child agents. */
export async function onLoad() {
  await tools.context.loaded;
  await ctx.registerTool("agent");
}
