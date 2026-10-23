// Types for `import ... from "wendy"` in wendy plugins. wendy strips types
// when it loads a plugin, so this file only serves editors and `tsc`.
//
//   import { definePlugin } from "wendy";
//   export default definePlugin({ name: "notes", tools: { ... } });
//
// A plugin is a set of tools. Each tool call is one IPO operation the kernel
// runs: input(raw) → process(input) → output(result). Plugins use node's
// modules (fs, path, child_process, ...) as usual; all of their IO goes
// through wendy's IO layer and is traced with the operation it belongs to.

declare module "wendy" {
  export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

  /** A file change a process made, observed on the read-write mounts. */
  export interface Change {
    op: "write" | "mkdir" | "remove";
    path: string;
    dir: boolean;
  }

  export interface ExecOptions {
    /** Working directory (relative to the plugin's cwd); must be on a disk mount. */
    cwd?: string;
    /** Added to the inherited environment (PATH, HOME, LANG, ...). */
    env?: Record<string, string>;
    /** Sent to the process's stdin. */
    input?: string | Uint8Array;
    /** Milliseconds before the process group is killed. */
    timeout?: number;
    /** Bytes kept per output stream (default 1 MiB). */
    maxBuffer?: number;
  }

  export interface ExecResult {
    code: number | null;
    signal: string | null;
    stdout: string;
    stderr: string;
    timedOut: boolean;
    durationMs: number;
    truncated: boolean;
    /** What the process changed on the read-write disk mounts. */
    changes: Change[];
    /** False when the OS dropped file events, so `changes` may be partial. */
    changesComplete: boolean;
  }

  export interface ToolInfo {
    name: string;
    description: string;
    input_schema: Json;
    plugin: string;
  }

  /** What a tool's phases receive: the SDK, bound to their operation. */
  export interface Context {
    /** Id of the operation (`op-12`), as it appears in the trace. */
    readonly op: string;
    readonly plugin: string;
    /** Run a bash command string, or an argv array, as a confined process. */
    exec(command: string | string[], options?: ExecOptions): Promise<ExecResult>;
    /** Run another tool as a child operation and wait for its output. */
    call<T = any>(tool: string, input?: Json): Promise<T>;
    /** Start another tool without waiting; resolves to its operation id. */
    wake(tool: string, input?: Json): Promise<string>;
    tools(): Promise<ToolInfo[]>;
    /** Write to the plugin's stderr (kept by wendy, traced). */
    log(...args: unknown[]): void;
  }

  export interface Tool<In = any, Mid = In, Out = Mid> {
    description?: string;
    /** JSON Schema of the raw input; shown to whoever calls the tool. */
    schema?: Json;
    /** Check and normalize the raw input (default: pass it through). */
    input?(raw: any, ctx: Context): In | Promise<In>;
    /** Do the work. */
    process(input: In, ctx: Context): Mid | Promise<Mid>;
    /** Shape the result for the caller (default: pass it through). */
    output?(result: Mid, ctx: Context): Out | Promise<Out>;
  }

  export interface PluginDefinition {
    /** Letters, digits, '-' and '_'. */
    name: string;
    description?: string;
    version?: string;
    /** Tool names match [a-zA-Z0-9_-]{1,64} and are global across plugins. */
    tools: Record<string, Tool>;
  }

  export function definePlugin<P extends PluginDefinition>(definition: P): P;

  /** Like ctx.exec, attributed to the operation in flight when unambiguous. */
  export function exec(command: string | string[], options?: ExecOptions): Promise<ExecResult>;
  export function call<T = any>(tool: string, input?: Json): Promise<T>;
  export function wake(tool: string, input?: Json): Promise<string>;
  export function tools(): Promise<ToolInfo[]>;
}
