import type { Cancellable, HostModel } from "ply:plugins";

export type ObjectValue = Record<string, unknown>;
export type Id = string | number;
export type Message = { jsonrpc: "2.0"; id?: Id | null; method?: string; params?: unknown; result?: unknown; error?: unknown };
export type Session = {
  id: string; cwd: string; model: string; effort: string; snapshot?: unknown; history: { role: "user" | "agent"; content: unknown }[];
};
export type Active = {
  session: Session; cancelled: boolean; call?: Cancellable<unknown>; tools: Map<string, { name: string; input: unknown }>;
};
export type Options = { stateDirectory?: string };
export type ConfigOption = { id: string; name: string; category: string; type: "select"; currentValue: string; options: { value: string; name: string }[] };
export type Catalog = HostModel[];
export type Permission = PlyApprovalRequest | PlyToolApprovalRequest;
export type Decision = { request_id: string; decision: "approved" | "rejected" | "pending"; reason: string };
