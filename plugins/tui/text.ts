

export const segmenter = typeof Intl !== "undefined" && typeof Intl.Segmenter === "function" ? new Intl.Segmenter(undefined, { granularity: "grapheme" }) : null;

export const chars = (text: string): string[] => segmenter ? Array.from(segmenter.segment(text), item => item.segment) : Array.from(text);

export function clean(text: string): string {
  return text.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g,
    c => "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0")).replace(/\t/g, "    ");
}

export function width(c: string): number {
  if (/^(?:\p{Mark}|\u200d|\ufe0f)+$/u.test(c)) return 0;
  const n = c.codePointAt(0)!;
  if (c.includes("\ufe0f") || c.includes("\u200d") || n >= 0x1f1e6 && n <= 0x1f1ff) return 2;
  return n >= 0x1100 && (n <= 0x115f || n >= 0x2e80 && n <= 0xa4cf || n >= 0xac00 && n <= 0xd7a3 || n >= 0xf900 && n <= 0xfaff || n >= 0xfe10 && n <= 0xfe6f || n >= 0xff00 && n <= 0xff60 || n >= 0xffe0 && n <= 0xffe6 || n >= 0x1f300) ? 2 : 1;
}

export function wrap(text: string, columns: number): string[] {
  const result: string[] = [];
  for (const line of clean(text).split("\n")) {
    let part = "", used = 0;
    for (const c of chars(line)) {
      const size = width(c);
      if (used + size > columns) { result.push(part); part = ""; used = 0; }
      part += c; used += size;
    }
    result.push(part);
  }
  return result;
}

export function oneLine(text: string, columns: number): string { return wrap(text.replace(/\n/g, " "), columns)[0] ?? ""; }

export function pad(text: string, columns: number): string {
  const fitted = oneLine(text, columns);
  return fitted + " ".repeat(Math.max(0, columns - chars(fitted).reduce((sum, c) => sum + width(c), 0)));
}
