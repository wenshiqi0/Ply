import type { Session } from "./types";

export function keys(s: Session, data: string): { key: string; paste?: boolean }[] {
  s.pendingKeys += data;
  if (data) s.lastKey = Date.now();
  const result: { key: string; paste?: boolean }[] = [];
  const names: Record<string, string> = { "\x1b[A": "up", "\x1b[B": "down", "\x1b[C": "right", "\x1b[D": "left", "\x1b[5~": "pageup", "\x1b[6~": "pagedown", "\x1b[H": "home", "\x1b[F": "end", "\x1b[1~": "home", "\x1b[4~": "end", "\x1b[3~": "delete", "\x1b[13;2u": "newline", "\x1b[27;2;13~": "newline", "\x1bOA": "up", "\x1bOB": "down", "\x1bOC": "right", "\x1bOD": "left", "\x1bOH": "home", "\x1bOF": "end" };
  while (s.pendingKeys) {
    if (s.paste) {
      const end = s.pendingKeys.indexOf("\x1b[201~");
      if (end < 0) { if (s.pendingKeys.length > 65536) { s.inputTooLong = true; s.pendingKeys = s.pendingKeys.slice(-6); } break; }
      result.push({ key: s.pendingKeys.slice(0, end), paste: true });
      s.pendingKeys = s.pendingKeys.slice(end + 6); s.paste = false; continue;
    }
    if (s.pendingKeys.startsWith("\x1b[200~")) { s.paste = true; s.pendingKeys = s.pendingKeys.slice(6); continue; }
    if (s.pendingKeys.startsWith("\x1b")) {
      if (s.pendingKeys.startsWith("\x1b\r") || s.pendingKeys.startsWith("\x1b\n")) { result.push({ key: "newline" }); s.pendingKeys = s.pendingKeys.slice(2); continue; }
      const sequence = /^(?:\x1b\[[0-?]*[ -/]*[@-~]|\x1bO[A-DHF])/.exec(s.pendingKeys)?.[0];
      if (sequence) { result.push({ key: names[sequence] ?? "ignored" }); s.pendingKeys = s.pendingKeys.slice(sequence.length); continue; }
      if (Date.now() - s.lastKey < 150) break;
      result.push({ key: "escape" }); s.pendingKeys = s.pendingKeys.slice(1); continue;
    }
    const c = String.fromCodePoint(s.pendingKeys.codePointAt(0)!);
    s.pendingKeys = s.pendingKeys.slice(c.length);
    result.push({ key: c === "\r" ? "enter" : c === "\n" ? "newline" : c === "\x03" ? "interrupt" : c === "\x04" ? "eof" : c === "\x7f" || c === "\b" ? "backspace" : c });
  }
  return result;
}
