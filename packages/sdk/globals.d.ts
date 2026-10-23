// What wendy adds to a plugin's world, for editors and `tsc`. wendy strips
// types when it loads a plugin; nothing here exists at runtime except
// `tools`.
//
// A plugin is a directory whose main.ts exports one function:
//
//   /** What it does (this comment is its description). */
//   export async function handle(input: { path: string }): Promise<{ lines: number }> {
//     const file = await tools.read({ path: input.path });   // another plugin
//     return { lines: file.total_lines };
//   }
//
// The directory name is the plugin's name; `handle`'s parameter and return
// types describe its input and output. Plugins use node's modules (fs,
// path, child_process, ...) as usual.

/**
 * Every loaded plugin, by name. `tools.read(input)` is a lazy fiber:
 * nothing runs until it is awaited, and awaiting it is the call (a child
 * fiber of the one awaiting). Awaiting the same handle again does not run
 * it again.
 */
declare const tools: {
  readonly [plugin: string]: <Out = any>(input?: unknown) => PromiseLike<Out>;
};
