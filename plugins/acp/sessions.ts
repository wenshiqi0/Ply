import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import path from "node:path";
import type { Catalog, ConfigOption, ObjectValue, Session } from "./types";
import { object, ProtocolError, text } from "./transport";

export class Sessions {
  private sessions = new Map<string, Session>();
  constructor(private directory: string, readonly catalog: Catalog) {}
  private file(id: string): string {
    if (!/^session-[a-z0-9-]{1,96}$/.test(id)) throw new ProtocolError(-32602, "invalid session id");
    return path.join(this.directory, id + ".json");
  }
  validate(input: ObjectValue): void {
    if (text(input.cwd) !== process.cwd()) throw new ProtocolError(-32602, "session cwd must equal the mounted process cwd");
    if (!Array.isArray(input.mcpServers) || input.mcpServers.length) throw new ProtocolError(-32602, "external MCP servers are not supported");
  }
  async create(input: ObjectValue): Promise<Session> {
    this.validate(input);
    const selected = this.catalog.find(row => row.isDefault) ?? this.catalog[0];
    if (!selected) throw new ProtocolError(-32000, "model catalog unavailable");
    const session: Session = { id: "session-" + Date.now().toString(36) + "-" + Math.random().toString(36).slice(2), cwd: process.cwd(), model: selected.id, effort: selected.defaultEffort, history: [] };
    await this.save(session); return session;
  }
  async load(input: ObjectValue): Promise<Session> {
    this.validate(input);
    const id = text(input.sessionId);
    if (this.sessions.has(id)) return this.get(id);
    const file = this.file(id);
    let saved: ObjectValue;
    try { saved = object(JSON.parse(await readFile(file, "utf8"))); }
    catch { throw new ProtocolError(-32002, "session not found"); }
    if (saved.format !== 1 || saved.id !== id || saved.cwd !== process.cwd()) throw new ProtocolError(-32602, "session cwd mismatch");
    const session: Session = { id, cwd: text(saved.cwd), model: text(saved.model), effort: typeof saved.effort === "string" ? saved.effort : text(saved.effort), snapshot: saved.snapshot, history: [] };
    if (!Array.isArray(saved.history)) throw new ProtocolError(-32602, "invalid session history");
    session.history = saved.history.map(raw => {
      const row = object(raw);
      if (row.role !== "user" && row.role !== "agent") throw new ProtocolError(-32602, "invalid session history role");
      return { role: row.role, content: row.content };
    });
    this.validateSelection(session); this.sessions.set(id, session); return session;
  }
  get(id: unknown): Session {
    const session = this.sessions.get(text(id));
    if (!session) throw new ProtocolError(-32002, "unknown session");
    return session;
  }
  async save(session: Session): Promise<void> {
    await mkdir(this.directory, { recursive: true, mode: 0o700 });
    const target = this.file(session.id), temporary = target + "." + Math.random().toString(36).slice(2) + ".tmp";
    await writeFile(temporary, JSON.stringify({ format: 1, ...session }), { mode: 0o600, flag: "wx" });
    await rename(temporary, target); this.sessions.set(session.id, session);
  }
  validateSelection(session: Session): void {
    const selected = this.catalog.find(row => row.id === session.model);
    if (!selected || !(selected.efforts.includes(session.effort) || selected.efforts.length === 0 && session.effort === "")) throw new ProtocolError(-32602, "model or effort unavailable");
  }
  config(session: Session): ConfigOption[] {
    const selected = this.catalog.find(row => row.id === session.model)!;
    return [
      { id: "model", name: "Model", category: "model", type: "select", currentValue: session.model, options: this.catalog.map(row => ({ value: row.id, name: row.name })) },
      { id: "reasoning_effort", name: "Reasoning", category: "thought_level", type: "select", currentValue: session.effort, options: selected.efforts.map(value => ({ value, name: value })) },
    ];
  }
  async select(session: Session, input: ObjectValue): Promise<void> {
    const value = text(input.value), id = text(input.configId);
    const updated = { ...session };
    if (id === "model") {
      updated.model = value;
      const selected = this.catalog.find(row => row.id === value);
      if (selected && !selected.efforts.includes(updated.effort)) updated.effort = selected.defaultEffort;
    } else if (id === "reasoning_effort") updated.effort = value;
    else throw new ProtocolError(-32602, "unknown config option");
    this.validateSelection(updated); await this.save(updated);
    Object.assign(session, updated);
  }
}
