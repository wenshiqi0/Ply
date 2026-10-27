import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const documents = ["README.md", "SPEC.md", "AGENTS.md", ...fs.readdirSync(path.join(root, "rfcs")).map(name => `rfcs/${name}`)];
const failures = [];
for (const document of documents) {
  const source = fs.readFileSync(path.join(root, document), "utf8").replace(/```[\s\S]*?```|`[^`]*`/g, "");
  for (const match of source.matchAll(/\]\(([^\s)]+)(?:\s+"[^"]*")?\)/g)) {
    const target = match[1].split("#")[0];
    if (!target || /^[a-z][a-z0-9+.-]*:/i.test(target)) continue;
    const resolved = path.resolve(root, path.dirname(document), decodeURIComponent(target));
    if (!fs.existsSync(resolved)) failures.push(`${document}: missing ${target}`);
  }
}
for (const name of fs.readdirSync(path.join(root, "plugins"))) {
  if (!fs.existsSync(path.join(root, "plugins", name, "types.ts"))) failures.push(`${name}: missing types.ts`);
}
const spec = fs.readFileSync(path.join(root, "SPEC.md"), "utf8");
for (const section of ["SPEC-PLUGINS", "SPEC-TYPES", "SPEC-FIBER", "SPEC-AGENT", "SPEC-CONTEXT", "SPEC-TOOLS", "SPEC-SKILLS-MEMORY"]) {
  if (!spec.includes(`## ${section}`)) failures.push(`SPEC.md: missing ${section}`);
}
if (failures.length) {
  process.stderr.write(failures.join("\n") + "\n");
  process.exitCode = 1;
} else console.log(`Checked ${documents.length} documents, local links and all plugin type files.`);
