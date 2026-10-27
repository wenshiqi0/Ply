declare module "ply:models" {
  export interface ModelProfile {
    provider: string;
    model: string;
    apiKeyEnv?: string;
    apiKeyHeader?: string;
    reasoningEffort?: string;
    definition?: Record<string, unknown>;
  }
  export interface ModelInfo { id: string; name?: string; contextWindow: number; maxTokens: number }
  export function request(input: { operation: "describe"; profile: ModelProfile }): Promise<ModelInfo>;
  export function request(input: {
    operation: "complete";
    profile: ModelProfile;
    context?: unknown;
    purpose?: "task" | "compaction";
    maxTokens?: number;
  }): Promise<import("../../../plugins/agent/types").Message>;
}

/** Host-private recovery records; independent of workspace write mounts. */
