declare module "ply:markdown" {
  export interface Node {
    type: string;
    children?: Node[];
    text?: string;
    level?: number;
    language?: string;
    start?: number | null;
    url?: string;
    title?: string;
    label?: string;
    checked?: boolean;
    alignments?: ("left" | "center" | "right" | "none")[];
  }
  export function parse(source: string): Node;
  /** Width is 8..240 cells; code content stays literal and links are displayed as text. */
  export function render(source: string | Node, options?: { columns?: number }): import("ply:terminal").Line[];
}


/** Internal interpreter used by the model-visible bash tool. */
