// Terminal presentation of the native CommonMark tree. Markdown stays data.
(function (host) {
  const segmenter = typeof Intl !== "undefined" && typeof Intl.Segmenter === "function" ? new Intl.Segmenter(undefined, { granularity: "grapheme" }) : null;
  const chars = text => segmenter ? Array.from(segmenter.segment(text), item => item.segment) : Array.from(text);
  const clean = text => text.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g, c => "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0")).replace(/\t/g, "    ");
  function cell(c) {
    if (/^[\p{Mark}\u200d\ufe0f]+$/u.test(c)) return 0;
    const n = c.codePointAt(0);
    if (c.includes("\ufe0f") || c.includes("\u200d") || n >= 0x1f1e6 && n <= 0x1f1ff) return 2;
    return n >= 0x1100 && (n <= 0x115f || n >= 0x2e80 && n <= 0xa4cf || n >= 0xac00 && n <= 0xd7a3 || n >= 0xf900 && n <= 0xfaff || n >= 0xfe10 && n <= 0xfe6f || n >= 0xff00 && n <= 0xff60 || n >= 0xffe0 && n <= 0xffe6 || n >= 0x1f300) ? 2 : 1;
  }
  const cells = text => chars(text).reduce((sum, c) => sum + cell(c), 0);
  const children = node => Array.isArray(node.children) ? node.children : [];
  const plain = node => typeof node.text === "string" ? node.text : node.type === "soft_break" || node.type === "hard_break" ? "\n" : children(node).map(plain).join("");
  function compact(runs) {
    const out = [];
    for (const run of runs) {
      if (!run.text) continue;
      const previous = out[out.length - 1];
      const same = previous && ["style", "bold", "italic", "underline", "strike"].every(key => previous[key] === run[key]);
      if (same) previous.text += run.text; else out.push({ ...run });
    }
    return out;
  }
  function render(source, options = {}) {
    const columns = options.columns ?? 80;
    if (!Number.isSafeInteger(columns) || columns < 8 || columns > 240) throw new TypeError("markdown columns must be in 8..240");
    const document = typeof source === "string" ? host.parse(source) : source;
    if (!document || typeof document !== "object") throw new TypeError("markdown needs text or a parsed document");
    let visited = 0;
    function guard(depth) { if (depth > 64 || ++visited > 200000) throw new Error("markdown render budget exceeded"); }
    function inline(nodes, attrs = {}, depth = 0) {
      const runs = [];
      for (const node of nodes) {
        guard(depth);
        switch (node.type) {
          case "text": case "html": runs.push({ ...attrs, text: clean(node.text ?? "") }); break;
          case "code": runs.push({ ...attrs, text: clean(node.text ?? ""), style: "code" }); break;
          case "soft_break": runs.push({ ...attrs, text: " " }); break;
          case "hard_break": runs.push({ ...attrs, text: "\n" }); break;
          case "strong": runs.push(...inline(children(node), { ...attrs, bold: true }, depth + 1)); break;
          case "emphasis": runs.push(...inline(children(node), { ...attrs, italic: true }, depth + 1)); break;
          case "strikethrough": runs.push(...inline(children(node), { ...attrs, strike: true }, depth + 1)); break;
          case "task": runs.push({ ...attrs, text: node.checked ? "☑ " : "☐ ", style: "accent" }); break;
          case "footnote_ref": runs.push({ ...attrs, text: "[" + clean(node.text ?? "") + "]", style: "muted" }); break;
          case "link": case "image": {
            const label = inline(children(node), { ...attrs, style: "accent", underline: true }, depth + 1);
            if (node.type === "image") runs.push({ ...attrs, text: "[图片] " });
            runs.push(...label);
            const url = clean(node.url ?? ""), caption = label.map(r => r.text).join("");
            if (url && url !== caption) runs.push({ ...attrs, text: " (" + url + ")", style: "muted" });
            break;
          }
          default: runs.push(...inline(children(node), attrs, depth + 1));
        }
      }
      return compact(runs);
    }
    function clipped(text, max) {
      let out = "", used = 0;
      for (const c of chars(clean(text))) { if (used + cell(c) > max) break; out += c; used += cell(c); }
      return out;
    }
    function wrapRuns(runs, limit, first = "", next = first, style = "normal") {
      first = clipped(first, Math.max(0, limit - 4)); next = clipped(next, Math.max(0, limit - 4));
      const out = []; let line = [], used = 0, prefix = first;
      const available = () => Math.max(1, limit - cells(prefix));
      function emit(units) {
        const content = compact(units.map(u => ({ ...u.run, text: u.text })));
        out.push({ spans: compact([{ text: prefix, style: "muted" }, ...content]), style }); prefix = next;
      }
      for (const run of runs) for (const c of chars(run.text)) {
        if (c === "\n") { emit(line); line = []; used = 0; continue; }
        const size = cell(c);
        if (used + size > available() && line.length) {
          let split = -1;
          for (let i = line.length - 1; i >= 0; i--) if (/^\s$/u.test(line[i].text) && line[i].run.style !== "code") { split = i; break; }
          if (split > 0) { emit(line.slice(0, split)); line = line.slice(split + 1); used = line.reduce((n, u) => n + cell(u.text), 0); }
          else { emit(line); line = []; used = 0; }
          if (used + size > available() && line.length) { emit(line); line = []; used = 0; }
        }
        if (!line.length && /^\s$/u.test(c) && run.style !== "code") continue;
        line.push({ text: c, run }); used += size;
      }
      if (line.length || !out.length) emit(line);
      return out;
    }
    const out = [];
    function blank() { if (out.length && out[out.length - 1] !== "") out.push(""); }
    function text(runs, prefix, continuation = prefix, style = "normal") { out.push(...wrapRuns(runs, columns, prefix, continuation, style)); }
    function literal(text, width) {
      const lines = []; let value = "", used = 0;
      for (const c of chars(clean(text))) { if (used + cell(c) > width) { lines.push(value); value = ""; used = 0; } value += c; used += cell(c); }
      lines.push(value); return lines;
    }
    function codeBlock(node, prefix) {
      const outer = Math.max(5, columns - cells(prefix)), width = outer - 4;
      const label = clipped(clean(node.language || "code"), Math.max(0, outer - 5));
      out.push({ text: prefix + "╭─ " + label + " " + "─".repeat(Math.max(0, outer - cells(label) - 5)) + "╮", style: "muted" });
      const value = plain(node).replace(/\n$/, "");
      for (const line of value.split("\n")) for (const part of literal(line, width)) {
        out.push({ spans: [{ text: prefix + "│ ", style: "muted" }, { text: part + " ".repeat(Math.max(0, width - cells(part))), style: "code" }, { text: " │", style: "muted" }] });
      }
      out.push({ text: prefix + "╰" + "─".repeat(outer - 2) + "╯", style: "muted" }); blank();
    }
    function table(node, prefix, depth) {
      const head = children(node).find(n => n.type === "table_head");
      const headCells = head ? children(head).flatMap(n => n.type === "table_row" ? children(n) : [n]).filter(n => n.type === "table_cell") : [];
      const body = children(node).filter(n => n.type === "table_row").map(n => children(n).filter(n => n.type === "table_cell"));
      const rows = headCells.length ? [headCells, ...body] : body;
      const count = Math.max(0, ...rows.map(row => row.length)); if (!count) return;
      const values = rows.map((row, index) => Array.from({ length: count }, (_, i) => inline(row[i] ? children(row[i]) : [], headCells.length && index === 0 ? { bold: true } : {}, depth + 1)));
      const space = columns - cells(prefix) - count * 3 - 1;
      if (space < count * 3) {
        const labels = headCells.map(n => plain(n));
        if (headCells.length && values.length === 1) { for (const label of labels) text([{ text: label, bold: true }], prefix); blank(); return; }
        for (let r = headCells.length ? 1 : 0; r < values.length; r++) {
          for (let c = 0; c < count; c++) text([{ text: (labels[c] || String(c + 1)) + ": ", bold: true }, ...values[r][c]], prefix);
          blank();
        }
        return;
      }
      const ideals = Array.from({ length: count }, (_, c) => Math.max(3, ...values.map(row => Math.min(60, cells(row[c].map(s => s.text).join("").replace(/\n/g, " "))))));
      const widths = Array(count).fill(3); let remaining = space - count * 3;
      while (remaining > 0) {
        let expanded = false;
        for (let c = 0; c < count && remaining > 0; c++) if (widths[c] < ideals[c]) { widths[c]++; remaining--; expanded = true; }
        if (!expanded) break;
      }
      function border(left, middle, right) { out.push({ text: prefix + left + widths.map(w => "─".repeat(w + 2)).join(middle) + right, style: "muted" }); }
      border("┌", "┬", "┐");
      for (let r = 0; r < values.length; r++) {
        const wrapped = values[r].map((runs, c) => wrapRuns(runs, widths[c]));
        const height = Math.max(...wrapped.map(lines => lines.length));
        for (let line = 0; line < height; line++) {
          const spans = [{ text: prefix + "│ ", style: "muted" }];
          for (let c = 0; c < count; c++) {
            const runs = wrapped[c][line]?.spans ?? [], used = cells(runs.map(s => s.text).join(""));
            const pad = Math.max(0, widths[c] - used), align = node.alignments?.[c];
            const left = align === "right" ? pad : align === "center" ? Math.floor(pad / 2) : 0;
            spans.push({ text: " ".repeat(left) }, ...runs, { text: " ".repeat(pad - left) }, { text: c === count - 1 ? " │" : " │ ", style: "muted" });
          }
          out.push({ spans: compact(spans) });
        }
        if (headCells.length && r === 0) border("├", "┼", "┤");
      }
      border("└", "┴", "┘"); blank();
    }
    const blockTypes = new Set(["paragraph", "heading", "blockquote", "list", "item", "code_block", "rule", "table", "html_block", "footnote"]);
    function block(node, prefix = "", style = "normal", depth = 0) {
      guard(depth); prefix = clipped(prefix, Math.max(0, columns - 8));
      switch (node.type) {
        case "heading": blank(); text(inline(children(node), { bold: true }, depth + 1), prefix, prefix, "title"); blank(); break;
        case "paragraph": text(inline(children(node), {}, depth + 1), prefix, prefix, style); blank(); break;
        case "blockquote": for (const child of children(node)) block(child, prefix + "│ ", "muted", depth + 1); blank(); break;
        case "code_block": codeBlock(node, prefix); break;
        case "rule": out.push({ text: prefix + "─".repeat(Math.max(1, columns - cells(prefix))), style: "muted" }); blank(); break;
        case "table": table(node, prefix, depth); break;
        case "list": {
          let number = node.start;
          for (const item of children(node)) {
            const marker = Number.isInteger(number) ? String(number++) + ". " : "• ";
            const continuation = prefix + " ".repeat(cells(marker));
            const nodes = [...children(item)]; let lead = [];
            if (nodes[0]?.type === "paragraph") lead = children(nodes.shift());
            else while (nodes.length && !blockTypes.has(nodes[0].type)) lead.push(nodes.shift());
            text(inline(lead, {}, depth + 1), prefix + marker, continuation, style);
            for (const child of nodes) block(child, continuation, style, depth + 1);
          }
          blank(); break;
        }
        case "html_block": text([{ text: clean(plain(node)) }], prefix, prefix, style); blank(); break;
        case "footnote": text([{ text: "[" + clean(node.label ?? "") + "]", style: "muted" }], prefix); for (const child of children(node)) block(child, prefix + "  ", style, depth + 1); break;
        default:
          if (children(node).length) for (const child of children(node)) block(child, prefix, style, depth + 1);
          else text(inline([node], {}, depth + 1), prefix, prefix, style);
      }
    }
    block(document);
    while (out[out.length - 1] === "") out.pop();
    return out;
  }
  return { parse: host.parse, render };
})
