import type * as markdown from "ply:markdown";
import type * as terminal from "ply:terminal";
import type { AgentResult, UsageSummary } from "ply:plugins";

export type Decision = PlyApprovalResponse;

export type Review = { request: PlyApprovalRequest | PlyToolApprovalRequest; resolve: (decision: Decision) => void; cancelled: boolean; offset: number; seenEnd: boolean; lines?: string[]; columns?: number };

/** The built-in agent contract plus display fallbacks for host replacements. */
export type DisplayResult = Partial<Pick<AgentResult, "text" | "status" | "approval_requests" | "total_usage">> & {
  approval?: unknown; content?: string; stdout?: string; stderr?: string; exit_code?: number | null;
};
export type Outcome = { output?: DisplayResult; error?: string };

export type HistoryEntry = { text: string; role: "user" | "assistant" | "system" | "error"; document?: markdown.Node };

export type InputRow = { text: string; start: number; end: number; cells: number };

export type Session = {
  id: number; columns: number; rows: number; dirty: boolean; model: string;
  approvalMode: "ask" | "risk" | "bypass";
  reviews: Review[]; history: HistoryEntry[]; input: string; caret: number; inputView: number; offset: number; busy: boolean; outcome?: Outcome;
  sent: string[]; recalled: number; draft: string; goalColumn?: number; notice: string;
  screenKey: string; startedAt: number; lastPulse: number;
  bodyHeight: number;
  historyLines?: terminal.Line[]; historyColumns?: number;
  pendingKeys: string; paste: boolean; lastKey: number; approved: number; rejected: number; tasks: number;
  inputTooLong: boolean;
  calls: number; inputTokens: number; outputTokens: number; cacheRead: number | null; usageComplete: boolean;
};

export type TuiInput = { prompt?: string };

export type TuiResult = { tasks: number; approved: number; rejected: number; usage: Pick<UsageSummary, "calls" | "input_tokens" | "output_tokens" | "cache_read_tokens" | "cache_hit_rate" | "complete"> };

export type TaskUsage = UsageSummary;
