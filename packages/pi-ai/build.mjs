// Build-time Node only. The checked-in output is embedded by Rust's
// include_str!; neither cargo builds nor Ply executions invoke Node.
import { build } from "esbuild";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.dirname(fileURLToPath(import.meta.url));
const output = path.resolve(root, "../../crates/ply-jsc/src/vendor/pi-ai.js");
await mkdir(path.dirname(output), { recursive: true });
const globals = "fetch, Headers, Response, Request, URL, URLSearchParams, AbortController, AbortSignal, ReadableStream, FormData";
const result = await build({
  absWorkingDir: root,
  entryPoints: ["entry.js"],
  outfile: output,
  bundle: true,
  platform: "browser",
  format: "iife",
  globalName: "PlyPiAI",
  target: "es2022",
  minify: true,
  legalComments: "linked",
  metafile: true,
  // Ply exposes node-shaped globals to its plugins. This bundle uses the
  // browser paths, independently of those globals, in a private Web scope.
  define: { process: "undefined", "globalThis.process": "undefined" },
  plugins: [{
    name: "embedded-api-key-auth",
    setup(build) {
      build.onLoad({ filter: /[\\/]pi-ai[\\/]dist[\\/]auth[\\/]context\.js$/ }, async () => ({
        contents: await readFile(path.join(root, "browser-auth-context.js"), "utf8"), loader: "js",
      }));
    },
  }],
  banner: { js: `(function(host) {\nconst globalThis = Object.create((0, eval)("globalThis"));\nlet ${globals};` },
  footer: { js: `\nconst web = PlyPiAI.createWeb(host);\n({ ${globals} } = web);\nObject.assign(globalThis, web);\nreturn Object.freeze({ request: PlyPiAI.createRuntime(host, web).request });\n})` },
});
const external = Object.values(result.metafile.outputs).flatMap(output => output.imports).filter(item => item.external);
if (external.length) throw new Error("embedded bundle has external imports: " + JSON.stringify(external));
const packages = new Map();
for (const input of Object.keys(result.metafile.inputs)) {
  if (!input.startsWith("node_modules/")) continue;
  const parts = input.split("/");
  const index = parts.lastIndexOf("node_modules");
  const count = index + (parts[index + 1].startsWith("@") ? 3 : 2);
  const directory = path.join(root, ...parts.slice(0, count));
  const info = JSON.parse(await readFile(path.join(directory, "package.json"), "utf8"));
  packages.set(info.name, { directory, ...info });
}
const dependencies = [];
const notices = ["Third-party software included in Ply's embedded model bundle.\n"];
for (const info of [...packages.values()].sort((a, b) => a.name.localeCompare(b.name, "en"))) {
  const files = (await readdir(info.directory, { withFileTypes: true })).filter(file => file.isFile() && /^(licen[cs]e|copying)/i.test(file.name));
  const licenseTexts = await Promise.all(files.map(file => readFile(path.join(info.directory, file.name), "utf8")));
  if (info.name === "@earendil-works/pi-ai") licenseTexts.push(await readFile(path.join(root, "licenses/pi-ai-0.82.1.txt"), "utf8"));
  if (!licenseTexts.length) throw new Error("missing license text for bundled dependency " + info.name);
  dependencies.push({ name: info.name, version: info.version, license: info.license });
  notices.push(`\n${info.name}@${info.version} (${info.license})\n\n${licenseTexts.join("\n")}`);
}
await writeFile(path.join(path.dirname(output), "pi-ai.dependencies.json"), JSON.stringify(dependencies, null, 2) + "\n");
await writeFile(path.join(path.dirname(output), "THIRD-PARTY-NOTICES.txt"), notices.join("\n"));
console.log(`Bundled pi-ai 0.82.1 for JSC: ${Object.values(result.metafile.outputs).reduce((sum, item) => sum + item.bytes, 0)} bytes; no external imports`);
