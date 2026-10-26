// What ply adds to a plugin's world, for editors and `tsc`. ply strips
// types when it loads a plugin. Import plugins from "ply:plugins";
// `ctx` and `fiber` remain globals.
//
// A plugin is a directory whose main.ts exports one or both hooks:
//
//   import plugins, { context } from "ply:plugins";
//   /** What it does (this comment is its description). */
//   export async function onHandle(input: { path: string }): Promise<{ lines: number }> {
//     const wc = await plugins.bash({ command: `wc -l < ${input.path}` }); // another plugin
//     return { lines: Number(wc.stdout.trim()) };
//   }
//
//   /** Runs once; the host may pass this plugin's JSON options. */
//   export async function onLoad(options?: unknown) {
//     await context.loaded;                // wait for another plugin to load
//     await context.registerTool("lines");
//     return { lineCount: (s: string) => s.split("\n").length };   // goes on ctx
//   }
//
// The directory name is the plugin's name; `onHandle`'s parameter and return
// types describe its input and output. Plugins use node's modules (fs,
// path, child_process, ...) as usual.

/** The live plugin directory. No global tools/plugins binding is installed. */
declare module "ply:plugins" {
export interface Plugin {
  /**
   * Its `onHandle`, as a lazy fiber: nothing runs until it is awaited, and
   * awaiting it is the call (a child fiber of the one awaiting). Awaiting the
   * same handle again does not run it again.
   */
  <Out = any>(input?: unknown): PromiseLike<Out>;
  /** Resolves when its `onLoad` has returned (rejects if it failed or never loads). */
  readonly loaded: Promise<void>;
  /** The JSDoc on its `onHandle` (or `onLoad`). */
  readonly description: string;
  /** With `onHandle`: JSON Schema of its arguments as a tool (always an object). */
  readonly parameters?: Record<string, unknown>;
  /** With `onHandle`: the input is not an object, so tool arguments carry it as `{ input }`. */
  readonly wrapsInput?: boolean;
  /** The owning plugin's onLoad data/method exports; methods are async RPCs. */
  readonly [member: string]: any;
}

/** Every loaded plugin, by name. Object.keys(plugins) reflects current loading state. */
const plugins: { readonly [plugin: string]: Plugin };
export default plugins;

export type ContextType = "rules" | "project" | "extensions" | "state" | "reference";
export interface ContextBlockInput { type: ContextType; key: string; content: string | null; conversation?: string }
export interface ContextBlock { source: string; type: ContextType; key: string; content: string | null; version: number }
export interface ContextAgentFacts { max_children: number; max_delegation_depth: number; report_sections: string[]; completion_marker: string }
export interface TaskContextInput { prompt: string; model: { contextWindow: number; maxTokens: number }; allowed_tools: string[]; facts: ContextAgentFacts }
export interface ContextPlugin extends Plugin {
  readonly Type: { readonly Rules: "rules"; readonly Project: "project"; readonly Extensions: "extensions"; readonly State: "state"; readonly Reference: "reference" };
  /** onLoad writes templates; running fibers write only their own conversation. */
  set(input: ContextBlockInput): Promise<{ changed: boolean; version?: number; scope?: "conversation" | "template" }>;
  blocks(id?: string): Promise<ContextBlock[]>;
  inspect(id: string): Promise<{ epoch: number; fixed_prefix_fingerprint: string; revision: number; fixed_order: { type: ContextType; source: string; key: string }[]; tool_order: string[]; pending_updates: unknown[] }>;
  registerTool(name: string): Promise<void>;
  useModel(profile: import("ply:models").ModelProfile): Promise<void>;
  currentModel(): Promise<import("ply:models").ModelProfile | null>;
  contextConfig(): Promise<any>;
  availableTools(): Promise<string[]>;
  openTask(input: TaskContextInput): Promise<string>;
  restoreTask(snapshot: unknown, input: Omit<TaskContextInput, "prompt">): Promise<string>;
  /** Lower-level manual contexts still use the same block lifecycle and budget. */
  openConversation(system: string, info?: { contextWindow: number; maxTokens: number }, allowed?: string[]): Promise<string>;
  appendMessages(id: string, messages: any[], observation?: { expected_revision: number; context_tokens?: number; tools_signature?: string }): Promise<any>;
  appendToolResults(id: string, outcomes: { id: string; name: string; output: unknown; isError: boolean; approval_requests?: unknown[] }[]): Promise<any>;
  repairReport(id: string, errors: string[], revision: number): Promise<any>;
  prepareContext(id: string, options?: { defer_compaction: boolean; agent_state?: PlyChildBudget }): Promise<any>;
  commitCompaction(id: string, revision: number, summary: string, summaryTokens?: number): Promise<any>;
  cancelCompaction(id: string, revision: number): Promise<void>;
  exportConversation(id: string): Promise<unknown>;
  restoreConversation(snapshot: unknown, info: { contextWindow: number; maxTokens: number }, allowed: string[], system?: string): Promise<string>;
  closeConversation(id?: string | null): Promise<void>;
}
/** Stable plugin handles. onLoad interfaces are resolved lazily on each handle. */
export const context: ContextPlugin;
export const agent: Plugin;
export const bash: Plugin;
export const create: Plugin;
export const edit: Plugin;
export const read: Plugin;
export const remove: Plugin;
export const approval: Plugin & { readonly [member: string]: any };
export const deepseek: Plugin;
export const mimo: Plugin;
export const tui: Plugin;
export const jev: Plugin;
export const skill: Plugin & { readonly catalog: import("ply:skills").SkillCatalog };

}

/** Plugin-owned host/UI entry points, registered during onLoad; not model tools. */
declare module "ply:commands" {
  export interface CommandInfo { name: string; description: string; plugin: string }
  export interface Command<Input = unknown, Output = unknown> {
    name: string;
    description?: string;
    handler: (input: Input) => Output | PromiseLike<Output>;
  }
  export function register<Input = unknown, Output = unknown>(command: Command<Input, Output>): Promise<void>;
  export function list(): Promise<CommandInfo[]>;
  /** Starts an invocation in the registered plugin's realm and awaits its child fiber. */
  export function run<Output = unknown>(name: string, input?: unknown): Promise<Output>;
}

/** The current onHandle invocation. Available only while a fiber is running. */
declare const fiber: {
  /** The kernel's identity and ancestry, nearest parent first. */
  info(): Promise<{ id: string; parent: string | null; ancestors: string[] }>;
  /** Emit JSON-serializable data to FiberHook / --record, tagged with this fiber's id. */
  record(data: unknown): Promise<void>;
  /** Install/narrow a cumulative direct-child limit before any model calls. Restore only increases the spent count. */
  limitChildren(plugin: string, limit: number, started?: number): Promise<PlyChildBudget>;
  /** Current cumulative budget for this fiber and plugin, or null if not installed. */
  childBudget(plugin: string): Promise<PlyChildBudget | null>;
};

interface PlyChildBudget { limit: number; started: number; remaining: number }

/**
 * What every plugin's `onLoad` returned, side by side (a later mount of the
 * same name overrides an earlier one). Plain members are copies taken when
 * that `onLoad` returned; functions run in the realm of the plugin that
 * mounted them, so calling one always returns a promise.
 */
declare const ctx: { readonly [member: string]: any };

/** SKILL.md parsing in JSC. IO/discovery is provided by the skill plugin. */
declare module "ply:skills" {
  export interface SkillInfo { name: string; description: string; path: string; directory: string }
  export interface SkillCatalog { skills: SkillInfo[]; warnings: { path: string; message: string }[] }
  export interface SkillDocument {
    name: string; description: string; body: string;
    license: string | null; compatibility: string | null;
    metadata: Record<string, unknown> | null; allowed_tools: string | null;
  }
  /** YAML frontmatter plus Markdown body; data only, at most 64 KiB. */
  export function parse(source: string): SkillDocument;
}

/** pi-ai embedded in this realm; DeepSeek and Xiaomi MiMo only. */
declare module "ply:models" {
  export interface ModelProfile {
    provider: string;
    model: string;
    apiKeyEnv?: string;
    apiKeyHeader?: string;
    reasoningEffort?: string;
    definition?: Record<string, unknown>;
  }
  export function request(input: {
    operation: "describe" | "complete";
    profile: ModelProfile;
    context?: unknown;
    purpose?: "task" | "compaction";
    maxTokens?: number;
  }): Promise<any>;
}

/** Host-private recovery records; independent of workspace write mounts. */
declare module "ply:checkpoints" {
  export function create(snapshot: unknown): Promise<string>;
  export function claim(id: string): Promise<{ lease: number; snapshot: unknown }>;
  export function update(lease: number, snapshot: unknown): Promise<void>;
  export function finish(lease: number, remove: boolean): Promise<void>;
}

/**
 * Runtime-owned cleanup after failure/interruption. The whole batch has a
 * two-second deadline; its pending work is cancelled when the batch ends.
 * Await cleanup work that must finish. Ordinary success uses your finally.
 */
declare module "ply:lifecycle" {
  export interface InvocationInfo { plugin: string | null; loading: boolean; host: boolean; client: string | null; fiber: string | null; ancestors: string[] }
  /** Native caller provenance, retained across awaits; null outside a host entry. */
  export function current(): InvocationInfo | null;
  export function onDispose(callback: () => void | Promise<void>): () => void;
}

/** Real foreground controlling terminal, owned and released by the calling fiber. Never reads piped stdin. */
declare module "ply:terminal" {
  export interface Size { columns: number; rows: number }
  export function open(): Size & { id: number };
  export type Style = "normal" | "muted" | "accent" | "title" | "warning" | "error" | "code";
  export interface Span { text: string; style?: Style; bold?: boolean; italic?: boolean; underline?: boolean; strike?: boolean }
  export type Line = string | Span | { spans: Span[]; style?: Style };
  /** Plain text with host-owned styles. Only changed rows are painted; controls are escaped. */
  export function render(id: number, lines: Line[], options?: { cursor?: { row: number; column: number } | null; clear?: boolean }): Size;
  export function flush(id: number): void;
  export function read(id: number, timeout_ms?: number): Promise<Size & { data: string }>;
  export function close(id: number): void;
}

/** Bounded CommonMark parser returning data only. HTML and URLs are never executed/fetched. */
declare module "ply:markdown" {
  export interface Node {
    type: string;
    children?: Node[];
    text?: string;
    level?: number;
    language?: string;
    start?: number | null;
    url?: string;
    title?: string;
    label?: string;
    checked?: boolean;
    alignments?: ("left" | "center" | "right" | "none")[];
  }
  export function parse(source: string): Node;
  /** Width is 8..240 cells; code content stays literal and links are displayed as text. */
  export function render(source: string | Node, options?: { columns?: number }): import("ply:terminal").Line[];
}


/** Internal interpreter used by the model-visible bash tool. */
declare module "ply:sh" {
  export interface Result {
    exit_code: number | null;
    signal: string | null;
    timed_out: boolean;
    stdout: string;
    stderr: string;
  }
  export function exec(request: { command: string; cwd?: string; timeout_ms?: number }): Promise<Result>;
}

/** Plugin-owned, cross-realm events. Declare/subscribe during onLoad. */
declare module "ply:events" {
  export interface Event<T = unknown> {
    id: string;
    name: string;
    source: { plugin: string; fiber: string | null };
    data: T;
  }
  /** Define a local name; returns the native plugin-qualified name. */
  export function define(name: string): Promise<string>;
  /** Await the provider's plugins.<plugin>.loaded before subscribing. */
  export function on<T = unknown, R = unknown>(name: string, callback: (event: Event<T>) => R | Promise<R>): Promise<number>;
  export function off(subscription: number): Promise<boolean>;
  /** Only the owning plugin emits; listeners run serially, in their own child fibers. */
  export function emit<T = unknown, R = unknown>(name: string, data: T, options?: { timeout_ms?: number }): Promise<R[]>;
  export function list(): Promise<{ name: string; plugin: string; subscribers: number }[]>;
}


/** Native JSON HTTP transport; host bearer keys are never returned to JS. */
declare module "ply:http" {
  export function hasCredentials(provider: string): boolean;
  export function requestJson(request: {
    url: string;
    body: unknown;
    auth_provider?: string;
    timeout_ms?: number;
  }): Promise<{ status: number; data: unknown }>;
}


/** Environment-neutral approval protocol. Adapters subscribe to approval.request. */
interface PlyApprovalRequest {
  schema_version: 1;
  id: string;
  tool: "bash";
  command: string;
  cwd: string;
  assessments: unknown[];
  source: { fiber: string; parent: string | null };
  requested_at: number;
}
interface PlyApprovalResponse {
  request_id: string;
  decision: "approved" | "rejected" | "pending";
  reason?: string;
  reference?: string;
  approver?: string;
}

/** Model tool call awaiting execution. Existing bash approval.request stays unchanged. */
interface PlyToolApprovalRequest {
  schema_version: 1;
  id: string;
  tool: string;
  input: unknown;
  cwd: string;
  source: { fiber: string; parent: string | null };
  requested_at: number;
}
