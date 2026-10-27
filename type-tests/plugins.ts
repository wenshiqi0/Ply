import plugins, { agent, bash, context, memory, read } from "ply:plugins";

/** Compile-only contracts: bodies are never executed by the test runner. */
export async function pluginContracts() {
  const file = await read({ path: "README.md", offset: 1, limit: 10 });
  const count: number = file.total_lines;
  const result = await bash({ command: "pwd" });
  const output: string = result.stdout;
  const task = await agent({ prompt: "Inspect", max_children: 0 });
  const tokens: number = task.total_usage.total_tokens;
  await plugins.read({ path: "README.md" });
  await memory({ action: "remember", key: "language", content: "Chinese" });
  await memory({ action: "recall", key: "language" });
  await memory({ action: "forget", id: "abcd-1234", expected_version: 1 });
  await context.set({ source: "memory", type: context.Type.Reference, key: "source", content: null });
  await context.set({ source: "memory", type: "reference", key: "source", content: null });
  const prepared = await context.prepareContext("conversation");
  const budget: number = prepared.budget.input_budget_tokens;
  return { count, output, tokens, budget };
}

export function invalidPluginContracts() {
  // @ts-expect-error read requires a path.
  read({});
  // @ts-expect-error Known default-directory handles retain parameter checking.
  plugins.bash({ command: 123 });
  // @ts-expect-error The return value is not untyped.
  const text: string = read({ path: "README.md" });
  // @ts-expect-error Child count is numeric.
  agent({ prompt: "Inspect", max_children: "two" });
  // @ts-expect-error Remember requires content.
  memory({ action: "remember", key: "language" });
  // @ts-expect-error Recall uses key, not id.
  memory({ action: "recall", id: "abcd-1234" });
  // @ts-expect-error Forget requires a version.
  memory({ action: "forget", id: "abcd-1234" });
  // @ts-expect-error Forget never takes key.
  memory({ action: "forget", id: "abcd-1234", expected_version: 1, key: "language" });
  // @ts-expect-error Forget locates scope internally.
  memory({ action: "forget", id: "abcd-1234", expected_version: 1, scope: "user" });
  const mixed = { action: "forget" as const, id: "abcd-1234", expected_version: 1, key: "language" };
  // @ts-expect-error Mixed selectors are also rejected through variables.
  memory(mixed);
  // @ts-expect-error Context types are centrally defined.
  context.set({ source: "skill", type: "catalog", key: "skills", content: "value" });
  // @ts-expect-error A contribution names its own plugin; the caller is never inferred.
  context.set({ type: "extensions", key: "skills", content: "value" });
  // @ts-expect-error Load order belongs to the kernel, not to handles.
  void plugins.read.loaded.then;
  // @ts-expect-error Dynamic onLoad members must be narrowed before calling.
  plugins.custom.unknownMethod({});
  return text;
}
