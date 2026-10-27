/** Typed handles use the same contracts as the corresponding plugin sources. */
declare module "ply:plugins" {
  export interface PluginMetadata {
    readonly description: string;
    readonly parameters?: Record<string, unknown>;
    readonly wrapsInput?: boolean;
  }
  /** Call is lazy: awaiting the handle runs the plugin's onHandle and returns its output. */
  export type PluginHandle<Input = unknown, Output = unknown> = {
    call(input: Input): PromiseLike<Output>;
  }["call"] & PluginMetadata;
  /** Unknown plugins keep dynamic member discovery; narrow members before use. */
  export type Plugin<Input = unknown, Output = unknown> = PluginHandle<Input, Output> & { readonly [member: string]: unknown };
  export type Remote<Exports> = {
    readonly [Key in keyof Exports]: Exports[Key] extends (...args: infer Args) => infer Result
      ? (...args: Args) => Promise<Awaited<Result>> : Exports[Key];
  };
  export type ExportsOf<Module extends { onLoad: (...args: never[]) => unknown }> = Remote<Awaited<ReturnType<Module["onLoad"]>>>;
  export type AgentPlugin = Plugin<import("../../plugins/agent/types").AgentInput, import("../../plugins/agent/types").AgentResult> & ExportsOf<typeof import("../../plugins/agent/main")>;
  export type BashPlugin = Plugin<import("../../plugins/bash/types").BashInput, import("../../plugins/bash/types").BashResult> & ExportsOf<typeof import("../../plugins/bash/main")>;
  export type CreatePlugin = Plugin<import("../../plugins/create/types").CreateInput, import("../../plugins/create/types").CreateResult> & ExportsOf<typeof import("../../plugins/create/main")>;
  export type EditPlugin = Plugin<import("../../plugins/edit/types").EditInput, import("../../plugins/edit/types").EditResult> & ExportsOf<typeof import("../../plugins/edit/main")>;
  export type ReadPlugin = Plugin<import("../../plugins/read/types").ReadInput, import("../../plugins/read/types").ReadResult> & ExportsOf<typeof import("../../plugins/read/main")>;
  export type RemovePlugin = Plugin<import("../../plugins/remove/types").RemoveInput, import("../../plugins/remove/types").RemoveResult> & ExportsOf<typeof import("../../plugins/remove/main")>;
  export type TuiPlugin = Plugin<import("../../plugins/tui/types").TuiInput, import("../../plugins/tui/types").TuiResult> & ExportsOf<typeof import("../../plugins/tui/main")>;

  export type ContextPlugin = Plugin<never, never> & ExportsOf<typeof import("../../plugins/context/main")>;
  export type ApprovalPlugin = Plugin<never, never> & ExportsOf<typeof import("../../plugins/approval/main")>;
  export type AgentInput = import("../../plugins/agent/types").AgentInput;
  export type BashInput = import("../../plugins/bash/types").BashInput;
  export type BashResult = import("../../plugins/bash/types").BashResult;
  export type CreateInput = import("../../plugins/create/types").CreateInput;
  export type CreateResult = import("../../plugins/create/types").CreateResult;
  export type EditInput = import("../../plugins/edit/types").EditInput;
  export type EditResult = import("../../plugins/edit/types").EditResult;
  export type ReadInput = import("../../plugins/read/types").ReadInput;
  export type ReadResult = import("../../plugins/read/types").ReadResult;
  export type RemoveInput = import("../../plugins/remove/types").RemoveInput;
  export type RemoveResult = import("../../plugins/remove/types").RemoveResult;
  export type TuiInput = import("../../plugins/tui/types").TuiInput;
  export type TuiResult = import("../../plugins/tui/types").TuiResult;
  export type MemoryResult = import("../../plugins/memory/types").MemoryResult;
  export type MemoryOptions = import("../../plugins/memory/types").MemoryOptions;
  export type SkillOptions = import("../../plugins/skill/types").SkillOptions;
  export type ContextConfig = import("../../plugins/context/types").ContextConfig;
  export type ModelProfile = import("../../plugins/context/types").ModelProfile;
  export type ModelLimits = import("../../plugins/context/types").ModelLimits;
  export type AgentResult = import("../../plugins/agent/types").AgentResult;
  export type UsageSummary = import("../../plugins/agent/types").UsageSummary;
  export type ToolAuthorization = import("../../plugins/approval/types").ToolAuthorization;
  export type CommandAuthorization = import("../../plugins/approval/types").CommandAuthorization;
  export type MemoryInput = import("../../plugins/memory/types").MemoryInput;
  export type MemoryPlugin = Plugin<MemoryInput, import("../../plugins/memory/types").MemoryResult> & ExportsOf<typeof import("../../plugins/memory/main")>;
  export type ContextType = import("../../plugins/context/types").ContextType;
  export type ContextBlockInput = import("../../plugins/context/types").BlockInput;
  export type ContextBlock = import("../../plugins/context/types").Block;
  export type ContextAgentFacts = import("../../plugins/context/types").AgentFacts;
  export type TaskContextInput = import("../../plugins/context/types").TaskInput;
  export const context: ContextPlugin;
  export const agent: AgentPlugin;
  export const bash: BashPlugin;
  export const create: CreatePlugin;
  export const edit: EditPlugin;
  export const read: ReadPlugin;
  export const remove: RemovePlugin;
  export const approval: ApprovalPlugin;
  export const memory: MemoryPlugin;
  export const tui: TuiPlugin;
  export const skill: Plugin<never, never> & ExportsOf<typeof import("../../plugins/skill/main")>;
  export const jev: Plugin<never, never> & ExportsOf<typeof import("../../plugins/jev/main")>;
  export const deepseek: Plugin<never, never>;
  export const mimo: Plugin<never, never>;
  export interface PluginDirectory {
    readonly [plugin: string]: Plugin;
    readonly context: ContextPlugin;
    readonly agent: AgentPlugin;
    readonly bash: BashPlugin;
    readonly create: CreatePlugin;
    readonly edit: EditPlugin;
    readonly read: ReadPlugin;
    readonly remove: RemovePlugin;
    readonly approval: ApprovalPlugin;
    readonly memory: MemoryPlugin;
    readonly tui: TuiPlugin;
    readonly skill: typeof skill;
    readonly jev: typeof jev;
    readonly deepseek: typeof deepseek;
    readonly mimo: typeof mimo;
  }
  const plugins: PluginDirectory;
  export default plugins;
}
