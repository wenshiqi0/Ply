import type { ModelProfile } from "ply:models";

// The loop and pi-ai model calls run in this plugin's JSC realm. The host
// supplies HTTP/TLS and streams; no external model process is started.
export type Block = { type: string; text?: string; id?: string; name?: string; arguments?: Record<string, unknown> };

export type Message = {
  role: string;
  content: unknown;
  stopReason?: string;
  errorMessage?: string;
  responseId?: string;
  usage?: unknown;
  approval_requests?: unknown[];
  timestamp?: number;
};

export type ToolOutcome = { id: string; name: string; output: unknown; isError: boolean; approval_requests?: unknown[] };

export type Context = { systemPrompt: string; messages: Message[]; tools: { name: string }[] };

export type Budget = { reserve_output_tokens: number; estimated_input_tokens: number; input_budget_tokens: number; [key: string]: unknown };

export type PreparedContext = {
  revision: number;
  tools_signature: string;
  budget: Budget;
  context?: Context;
  compaction?: { context: Context; max_output_tokens: number; estimated_input_tokens: number; messages_to_summarize: number };
};

export type TokenUsage = {
  input_tokens: number;
  uncached_input_tokens: number | null;
  output_tokens: number;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  reasoning_tokens: number | null;
  total_tokens: number;
  cache_hit_rate: number | null;
};

export type UsageCounts = {
  calls: number;
  reported_calls: number;
  missing_calls: number;
  input_tokens: number;
  uncached_input_tokens: number | null;
  output_tokens: number;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  reasoning_tokens: number | null;
  total_tokens: number;
};

export type UsageSummary = {
  calls: number;
  reported_calls: number;
  missing_calls: number;
  complete: boolean;
  input_tokens: number;
  uncached_input_tokens: number | null;
  output_tokens: number;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  reasoning_tokens: number | null;
  total_tokens: number;
  cache_hit_rate: number | null;
};

/** One agent run: its usage, limits and direct children, linked to the run that delegated it. */
export type RunUsage = { own: UsageCounts; total: UsageCounts; parent?: RunUsage; policy: AgentPolicy; report: ReportState; compaction_retry_after: number;
  children: { limit: number; started: number }; conversation?: string };

export type AgentPolicy = { allowed_tools: string[]; max_delegation_depth: number; max_children: number };

export type AgentInput = {
  /** A self-contained task. Required for a new agent; omit when resuming. */
  prompt?: string;
  /** Resume a private recovery record returned by a failed agent. */
  resume_id?: string;
  /** Model-visible tool whitelist; children can only narrow their parent's list. */
  allowed_tools?: string[];
  /** Child generations this agent may create. Root default 1; children inherit remaining depth minus 1. Zero forbids delegation. */
  max_delegation_depth?: number;
  /** Cumulative direct-child agent limit, including failures/replacements/resumes (root default 2). Children may only narrow it. */
  max_children?: number;
  /** Required Markdown report section headings, inherited by children and retained on resume. */
  report_sections?: string[];
  /** Optional positive limit on task-model turns in this invocation. */
  max_turns?: number;
  /** Filled in by a delegating parent agent; any value a model supplies is replaced. */
  delegation?: string;
};

export type Recovery = {
  format: 4;
  phase: "ready" | "tools_pending";
  cwd: string;
  profile: ModelProfile;
  policy: AgentPolicy;
  children_started: number;
  report: ReportState;
  conversation: unknown;
  next_turn: number;
  tool_calls: number;
  previous_usage: UsageSummary;
  previous_total_usage: UsageSummary;
  pending_tools?: Block[];
};

export type ReportState = { required_sections: string[]; rejected_reports: number };

export type AgentResult = {
  text: string; model: string; turns: number; total_turns: number; tool_calls: number;
  status?: "approval_required"; approval_requests?: unknown[];
  usage: UsageSummary; total_usage: UsageSummary; policy: AgentPolicy; resumed_from?: string;
  delegation: { limit: number; started: number; remaining: number };
  report_validation: ReportState;
  previous_usage?: UsageSummary; previous_total_usage?: UsageSummary;
};
