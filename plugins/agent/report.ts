import type { AgentInput, ReportState, RunUsage } from "./types";

export const REPORT_END = "[[PLY_REPORT_COMPLETE]]";

export function sectionNames(value: unknown, name: string): string[] {
  if (!Array.isArray(value) || value.length > 16 || value.some(section => typeof section !== "string" || !section.trim() || section.length > 128 || /[\r\n]/.test(section))) {
    throw new Error(name + " must contain at most 16 non-empty, single-line section headings");
  }
  return [...new Set(value.map(section => (section as string).trim()))];
}

export function reportFor(input: AgentInput, parent?: RunUsage, saved?: ReportState): ReportState {
  const defaults = sectionNames(JSON.parse(process.env.PLY_AGENT_REPORT_SECTIONS ?? "[]"), "default report sections");
  const requested = input.report_sections === undefined ? (parent || saved ? [] : defaults) : sectionNames(input.report_sections, "report_sections");
  return { required_sections: sectionNames([...new Set([...(parent?.report.required_sections ?? []), ...(saved?.required_sections ?? []), ...requested])], "combined report sections"),
    rejected_reports: saved?.rejected_reports ?? 0 };
}

export function headingName(value: string): string {
  return value.trim().replace(/^\d+[.)、\s]+/, "").replace(/^[一二三四五六七八九十]+[、.)]\s*/, "").trim().toLowerCase();
}

export function validateReport(text: string, stop: string | undefined, report: ReportState): { text: string; errors: string[] } {
  const errors: string[] = [];
  const trimmed = text.trim();
  const lines = trimmed.split(/\r?\n/);
  if (lines.at(-1)?.trim() !== REPORT_END) errors.push("missing_completion_marker");
  else lines.pop();
  const body = lines.join("\n").trim();
  if (!body) errors.push("empty_report");
  if (stop !== "stop") errors.push(stop === "length" ? "truncated_report" : "unexpected_stop_reason");
  const headings: { name: string; line: number; depth: number }[] = [];
  const meaningful: boolean[] = [];
  let fence: { char: string; length: number } | undefined;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    meaningful[i] = false;
    const marker = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line);
    if (fence) {
      if (marker && marker[1][0] === fence.char && marker[1].length >= fence.length && !marker[2].trim()) fence = undefined;
      else meaningful[i] = !!line.trim();
      continue;
    }
    if (marker) { fence = { char: marker[1][0], length: marker[1].length }; continue; }
    const visible = line.replace(/(`+)[^`]*\1/g, "");
    if (visible.includes(REPORT_END)) errors.push("unexpected_completion_marker");
    if (/<\/?[｜|]{2}DSML[｜|]{2}[^>\n]*>/iu.test(visible) || /<[｜|]tool[_▁][^>\n]*[｜|]>/iu.test(visible)) errors.push("residual_protocol_tokens");
    const heading = /^ {0,3}(#{1,6})\s+(.+?)\s*#*\s*$/.exec(line);
    if (heading) headings.push({ name: headingName(heading[2]), line: i, depth: heading[1].length });
    else meaningful[i] = !!line.trim();
  }
  if (fence) errors.push("unclosed_code_fence");
  for (const required of report.required_sections) {
    const candidates = headings.filter(heading => heading.name === headingName(required));
    if (!candidates.length) { errors.push("missing_section:" + required); continue; }
    const nonempty = candidates.some(heading => {
      const end = headings.find(next => next.line > heading.line && next.depth <= heading.depth)?.line ?? lines.length;
      return meaningful.slice(heading.line + 1, end).some(Boolean);
    });
    if (!nonempty) errors.push("empty_section:" + required);
  }
  return { text: body, errors: [...new Set(errors)] };
}
