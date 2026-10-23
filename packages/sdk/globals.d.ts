// What wendy adds to a plugin's world, for editors and `tsc`. wendy strips
// types when it loads a plugin; at runtime there is `tools`, `ctx` and `fiber`.
//
// A plugin is a directory whose main.ts exports one or both hooks:
//
//   /** What it does (this comment is its description). */
//   export async function onHandle(input: { path: string }): Promise<{ lines: number }> {
//     const wc = await tools.bash({ command: `wc -l < ${input.path}` });   // another plugin
//     return { lines: Number(wc.stdout.trim()) };
//   }
//
//   /** Runs once, when the plugin loads. */
//   export async function onLoad() {
//     await tools.context.loaded;           // wait for another plugin to load
//     await ctx.registerTool("lines");      // use what it put on ctx
//     return { lineCount: (s: string) => s.split("\n").length };   // goes on ctx
//   }
//
// The directory name is the plugin's name; `onHandle`'s parameter and return
// types describe its input and output. Plugins use node's modules (fs,
// path, child_process, ...) as usual.

/** One plugin, as `tools.<name>`. */
interface WendyTool {
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

/** Every loaded plugin, by name. `Object.keys(tools)` lists them. */
declare const tools: { readonly [plugin: string]: WendyTool };

/** The current onHandle invocation. Available only while a fiber is running. */
declare const fiber: {
  /** The kernel's identity and ancestry, nearest parent first. */
  info(): Promise<{ id: string; parent: string | null; ancestors: string[] }>;
  /** Emit JSON-serializable data to FiberHook / --record, tagged with this fiber's id. */
  record(data: unknown): Promise<void>;
};

/**
 * What every plugin's `onLoad` returned, side by side (a later mount of the
 * same name overrides an earlier one). Plain members are copies taken when
 * that `onLoad` returned; functions run in the realm of the plugin that
 * mounted them, so calling one always returns a promise.
 */
declare const ctx: { readonly [member: string]: any };
