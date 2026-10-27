

export type Scope = "user" | "project" | "all";

export type Entry = { id: string; key: string; content: string; scope: string; version: number; created_at: number; updated_at: number; source: unknown };

export type Snapshot = { format: number; project_id: string | null; revisions: Record<string, number>; entries: Entry[] };

export type MemoryToolInput = {
  action: "remember" | "recall" | "forget";
  /** remember/recall only: user = personal; project = repository; all = recall only. Omit for forget. */
  scope?: Scope;
  /** remember/recall only: stable note label. Required for remember; omit to browse recall. Never pass to forget. */
  key?: string;
  /** Verbatim durable information explicitly requested by the user, up to 16 KiB. */
  content?: string;
  /** forget only: exact entry ID returned by recall. Never use id for recall. */
  id?: string;
  /** Current entry version from recall, required for replacement and forgetting. */
  expected_version?: number;
  /** Recall pagination offset (default 0). */
  offset?: number;
  /** Recall result count, 1..20 (default 8). An exact key selects detailed content. */
  limit?: number;
  /** With key: zero-based character offset for long content. */
  content_offset?: number;
  /** With key: content characters per page, 1..8000 (default 4096). */
  content_limit?: number;
};

export type MemoryOptions = { directory?: string; max_context_bytes?: number };

/** Model callers cannot mix key lookup and ID deletion selectors. */
export type MemoryInput = {
  action: "remember"; id?: never; key: string; content: string; scope?: "user" | "project"; expected_version?: number;
} | {
  action: "recall"; id?: never; content?: never; key?: string; scope?: Scope; expected_version?: number;
  offset?: number; limit?: number; content_offset?: number; content_limit?: number;
} | {
  action: "forget"; key?: never; scope?: never; content?: never; id: string; expected_version: number;
};

export type Mutation = { changed: boolean; id: string; scope: string | null; key?: string; version?: number; revision?: number; cleanup_pending: boolean };
export type RecallEntry = Entry & { content_offset: number; next_content_offset: number; total_content_chars: number; truncated: boolean; has_more_content: boolean };
export type RecallPacket = { project_id: string | null; revisions: Record<string, number>; total_matches: number; entries: RecallEntry[]; has_more: boolean; next_offset: number };
export type MemoryResult = Mutation | RecallPacket;
