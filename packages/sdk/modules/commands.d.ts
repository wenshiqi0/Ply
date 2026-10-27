declare module "ply:commands" {
  export interface CommandInfo { name: string; description: string; plugin: string }
  export interface Command<Input = unknown, Output = unknown> {
    name: string;
    description?: string;
    handler: (input: Input) => Output | PromiseLike<Output>;
  }
  export function register<Input = unknown, Output = unknown>(command: Command<Input, Output>): Promise<void>;
  export function list(): Promise<CommandInfo[]>;
  /** Runs the command in its plugin's realm, under the caller, and awaits it. */
  export function run<Output = unknown>(name: string, input?: unknown): Promise<Output>;
}
