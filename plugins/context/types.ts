

export const ContextType = { Rules: "rules", Project: "project", Extensions: "extensions", State: "state", Reference: "reference" } as const;
export type ContextType = typeof ContextType[keyof typeof ContextType];

// Model context and compaction policy. State is private to each conversation;
// model execution and usage accounting stay in the agent plugin.
export type Message = { role: string; content: unknown; timestamp?: number; [key: string]: unknown };

export type ModelProfile = {
  provider: string;
  model: string;
  apiKeyEnv?: string;
  apiKeyHeader?: string;
  reasoningEffort?: string;
  definition?: Record<string, unknown>;
};

export type ToolDef = { name: string; description: string; parameters: unknown };

export type ModelLimits = { contextWindow: number; maxTokens: number };

export type ContextConfig = {
  max_context_tokens: number;
  reserve_output_tokens: number;
  compaction: {
    enabled: boolean;
    trigger_ratio: number;
    target_ratio: number;
    keep_recent_turns: number;
    summary_max_tokens: number;
    max_retries: number;
    retry_delay_ms: number;
    failure_cooldown_turns: number;
  };
};

export type Limits = {
  configured_context_tokens: number;
  model_context_tokens: number | null;
  max_context_tokens: number;
  reserve_output_tokens: number;
  input_budget_tokens: number;
  trigger_tokens: number;
  target_tokens: number;
  summary_max_tokens: number;
  keep_recent_turns: number;
  compaction_enabled: boolean;
};

export type Plan = { revision: number; end: number; messages: number };

export type Block = { source: string; key: string; type: ContextType; content: string | null; version: number };

/** `source` is the contributing plugin's own name; without `conversation` the template changes. */
export type BlockInput = { source: string; type: ContextType; key: string; content: string | null; conversation?: string; provider?: string };

export type Provider = { source: string; key: string; type: ContextType; method: string };

export type AgentFacts = { max_children: number; max_delegation_depth: number; report_sections: string[]; completion_marker: string };

/** `inherit` names the parent conversation whose context a child task starts from; null or absent means none. */
export type TaskInput = { prompt: string; model: ModelLimits; allowed_tools: string[]; facts: AgentFacts; inherit?: string };

export type Conversation = {
  system: string;
  base: string;
  blocks: Map<string, Block>;
  pending: Map<string, Block>;
  tools: ToolDef[];
  /** Started from a parent's snapshot: its providers are not refreshed here. */
  inherited: boolean;
  facts?: AgentFacts;
  epoch: number;
  state_snapshot?: Message;
  messages: Message[];
  revision: number;
  limits: Limits;
  summary?: { text: string; timestamp: number; tokens: number };
  anchor?: { length: number; tokens: number; tools: string };
  plan?: Plan;
  compactions: number;
  compacting: boolean;
  allowed_tools?: string[];
};
