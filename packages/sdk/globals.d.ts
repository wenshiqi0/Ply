// What wendy adds to a plugin's world, for editors and `tsc`. wendy strips
// types when it loads a plugin. Import plugins from "wendy:plugins";
// `ctx` and `fiber` remain globals.
//
// A plugin is a directory whose main.ts exports one or both hooks:
//
//   import plugins from "wendy:plugins";
//   /** What it does (this comment is its description). */
//   export async function onHandle(input: { path: string }): Promise<{ lines: number }> {
//     const wc = await plugins.bash({ command: `wc -l < ${input.path}` }); // another plugin
//     return { lines: Number(wc.stdout.trim()) };
//   }
//
//   /** Runs once, when the plugin loads. */
//   export async function onLoad() {
//     await plugins.context.loaded;        // wait for another plugin to load
//     await ctx.registerTool("lines");      // use what it put on ctx
//     return { lineCount: (s: string) => s.split("\n").length };   // goes on ctx
//   }
//
// The directory name is the plugin's name; `onHandle`'s parameter and return
// types describe its input and output. Plugins use node's modules (fs,
// path, child_process, ...) as usual.

/** The live plugin directory. No global tools/plugins binding is installed. */
declare module "wendy:plugins" {
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
}

/** Every loaded plugin, by name. Object.keys(plugins) reflects current loading state. */
const plugins: { readonly [plugin: string]: Plugin };
export default plugins;
}

/** Plugin-owned host/UI entry points, registered during onLoad; not model tools. */
declare module "wendy:commands" {
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
  limitChildren(plugin: string, limit: number, started?: number): Promise<WendyChildBudget>;
  /** Current cumulative budget for this fiber and plugin, or null if not installed. */
  childBudget(plugin: string): Promise<WendyChildBudget | null>;
};

interface WendyChildBudget { limit: number; started: number; remaining: number }

/**
 * What every plugin's `onLoad` returned, side by side (a later mount of the
 * same name overrides an earlier one). Plain members are copies taken when
 * that `onLoad` returned; functions run in the realm of the plugin that
 * mounted them, so calling one always returns a promise.
 */
declare const ctx: { readonly [member: string]: any };

/** pi-ai embedded in this realm; DeepSeek and Xiaomi MiMo only. */
declare module "wendy:models" {
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
declare module "wendy:checkpoints" {
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
declare module "wendy:lifecycle" {
  export function onDispose(callback: () => void | Promise<void>): () => void;
}

/** Real foreground controlling terminal, owned and released by the calling fiber. Never reads piped stdin. */
declare module "wendy:terminal" {
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
declare module "wendy:markdown" {
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
  export function render(source: string | Node, options?: { columns?: number }): import("wendy:terminal").Line[];
}


/** Internal interpreter used by the model-visible bash tool. */
declare module "wendy:sh" {
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
declare module "wendy:events" {
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
declare module "wendy:http" {
  export function hasCredentials(provider: string): boolean;
  export function requestJson(request: {
    url: string;
    body: unknown;
    auth_provider?: string;
    timeout_ms?: number;
  }): Promise<{ status: number; data: unknown }>;
}


/** Environment-neutral approval protocol. Adapters subscribe to approval.request. */
interface WendyApprovalRequest {
  schema_version: 1;
  id: string;
  tool: "bash";
  command: string;
  cwd: string;
  assessments: unknown[];
  source: { fiber: string; parent: string | null };
  requested_at: number;
}
interface WendyApprovalResponse {
  request_id: string;
  decision: "approved" | "rejected" | "pending";
  reason?: string;
  reference?: string;
  approver?: string;
}

/** Model tool call awaiting execution. Existing bash approval.request stays unchanged. */
interface WendyToolApprovalRequest {
  schema_version: 1;
  id: string;
  tool: string;
  input: unknown;
  cwd: string;
  source: { fiber: string; parent: string | null };
  requested_at: number;
}
