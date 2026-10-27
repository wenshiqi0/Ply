/** Private storage primitive. Only the registered memory plugin realm may use it. */
declare module "ply:memory-store" {
  export function configure(options: { directory?: string }): Promise<{ directory: string; format: number }>;
  export function recall(options?: { scope?: "user" | "project" | "all" }): Promise<import("../../../plugins/memory/types").Snapshot>;
  export function remember(options: unknown): Promise<import("../../../plugins/memory/types").Mutation>;
  export function forget(options: unknown): Promise<import("../../../plugins/memory/types").Mutation>;
}
