// What wendy adds to a plugin's world, for editors and `tsc`. wendy strips
// types when it loads a plugin; nothing here exists at runtime except
// `tools`.
//
// A plugin is a directory whose main.ts exports one function:
//
//   /** What it does (this comment is its description). */
//   export async function handle(input: { path: string }): Promise<{ lines: number }> {
//     const wc = await tools.bash({ command: `wc -l < ${input.path}` });   // another plugin
//     return { lines: Number(wc.stdout.trim()) };
//   }
//
// The directory name is the plugin's name; `handle`'s parameter and return
// types describe its input and output. Plugins use node's modules (fs,
// path, child_process, ...) as usual.

/** One plugin, as `tools.<name>`. */
interface WendyTool {
  /**
   * A lazy fiber: nothing runs until it is awaited, and awaiting it is the
   * call (a child fiber of the one awaiting). Awaiting the same handle again
   * does not run it again.
   */
  <Out = any>(input?: unknown): PromiseLike<Out>;
  /** The JSDoc on the plugin's `handle`. */
  readonly description: string;
  /** JSON Schema of its arguments as a tool (always an object). */
  readonly parameters: Record<string, unknown>;
  /** The plugin's input is not an object, so tool arguments carry it as `{ input }`. */
  readonly wrapsInput: boolean;
}

/** Every loaded plugin, by name. `Object.keys(tools)` lists them. */
declare const tools: { readonly [plugin: string]: WendyTool };
