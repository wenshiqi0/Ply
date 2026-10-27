import type { InputRow, Session } from "./types";
import { chars, clean, width } from "./text";

export function inputLayout(text: string, columns: number, caret: number) {
  const value = chars(text), rows: InputRow[] = [{ text: "", start: 0, end: 0, cells: 0 }];
  for (let i = 0; i < value.length; i++) {
    let row = rows[rows.length - 1]; const c = value[i];
    if (c === "\n") { row.end = i; rows.push({ text: "", start: i + 1, end: i + 1, cells: 0 }); continue; }
    if (row.cells + width(c) > columns) { row.end = i; row = { text: "", start: i, end: i, cells: 0 }; rows.push(row); }
    row.text += c; row.cells += width(c); row.end = i + 1;
    if (row.cells === columns && value[i + 1] !== "\n") rows.push({ text: "", start: i + 1, end: i + 1, cells: 0 });
  }
  let row = 0;
  for (let i = 0; i < rows.length; i++) if (rows[i].start <= caret) row = i;
  const column = value.slice(rows[row].start, caret).reduce((sum, c) => sum + width(c), 0);
  return { rows, row, column };
}

export function insert(s: Session, text: string) {
  text = clean(text);
  const value = chars(s.input), prefix = value.slice(0, s.caret).join("");
  const next = prefix + text + value.slice(s.caret).join("");
  if (next.length > 65536) { s.inputTooLong = true; s.notice = "输入超过 65536 字符；这段内容未插入，Ctrl+U 清空后重试"; return; }
  s.input = next; s.caret = chars(prefix + text).length; s.goalColumn = undefined; s.notice = "";
}

export function remove(s: Session, from: number, to: number) {
  const value = chars(s.input);
  s.input = value.slice(0, from).join("") + value.slice(to).join(""); s.caret = from; s.goalColumn = undefined;
}

export function lineBounds(s: Session) {
  const value = chars(s.input); let start = s.caret, end = s.caret;
  while (start > 0 && value[start - 1] !== "\n") start--;
  while (end < value.length && value[end] !== "\n") end++;
  return { start, end };
}

export function recall(s: Session, direction: number) {
  if (!s.sent.length) return;
  if (s.recalled === s.sent.length) s.draft = s.input;
  s.recalled = Math.max(0, Math.min(s.sent.length, s.recalled + direction));
  s.input = s.recalled === s.sent.length ? s.draft : s.sent[s.recalled];
  s.caret = chars(s.input).length; s.goalColumn = undefined; s.notice = "";
}

export function vertical(s: Session, direction: number) {
  const layout = inputLayout(s.input, Math.max(1, s.columns - 7), s.caret), target = layout.row + direction;
  if (target < 0 || target >= layout.rows.length) { recall(s, direction); return; }
  const goal = s.goalColumn ?? layout.column, row = layout.rows[target], value = chars(s.input);
  let i = row.start, column = 0;
  while (i < row.end && column + width(value[i]) <= goal) column += width(value[i++]);
  s.caret = i; s.goalColumn = goal;
}

export function edit(s: Session, key: string, paste?: boolean) {
  const count = chars(s.input).length;
  if (paste) { insert(s, clean(key.replace(/\r\n?/g, "\n"))); return; }
  if (key === "left" || key === "\x02") s.caret = Math.max(0, s.caret - 1);
  else if (key === "right" || key === "\x06") s.caret = Math.min(count, s.caret + 1);
  else if (key === "home" || key === "\x01") s.caret = lineBounds(s).start;
  else if (key === "end" || key === "\x05") s.caret = lineBounds(s).end;
  else if (key === "up" || key === "\x10") { vertical(s, -1); return; }
  else if (key === "down" || key === "\x0e") { vertical(s, 1); return; }
  else if (key === "backspace") {
    if (s.caret > 0) remove(s, s.caret - 1, s.caret);
  } else if (key === "delete" || key === "eof") remove(s, s.caret, Math.min(count, s.caret + 1));
  else if (key === "\x15") { remove(s, lineBounds(s).start, s.caret); s.inputTooLong = false; s.notice = ""; }
  else if (key === "\x0b") remove(s, s.caret, lineBounds(s).end);
  else if (key === "\x17") {
    const value = chars(s.input); let start = s.caret;
    while (start > 0 && /\s/u.test(value[start - 1])) start--;
    while (start > 0 && !/\s/u.test(value[start - 1])) start--;
    remove(s, start, s.caret);
  } else if (key === "newline") insert(s, "\n");
  else if (!['ignored', 'escape', 'enter'].includes(key) && !/[\x00-\x1f]/.test(key)) insert(s, key);
  s.goalColumn = undefined;
}
