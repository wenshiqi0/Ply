/** Real foreground controlling terminal, owned by the calling call and released when it ends. Never reads piped stdin. */
declare module "ply:terminal" {
  export interface Size { columns: number; rows: number }
  export function open(): Size & { id: number };
  export type Style = "normal" | "muted" | "accent" | "title" | "warning" | "error" | "code";
  export interface Span { text: string; style?: Style; bold?: boolean; italic?: boolean; underline?: boolean; strike?: boolean }
  export type Line = string | Span | { spans: Span[]; style?: Style };
  /** Plain text with host-owned styles. Only changed rows are painted; controls are escaped. */
  export function render(id: number, lines: Line[], options?: { cursor?: { row: number; column: number } | null; clear?: boolean }): Size;
  export function flush(id: number): void;
  export function read(id: number, timeout_ms?: number): Promise<Size & { data: string }>;
  export function close(id: number): void;
}

/** Bounded CommonMark parser returning data only. HTML and URLs are never executed/fetched. */
