// Ply's own declarations stay fully checked. The root tsconfig skips d.ts
// files (skipLibCheck), because third-party ones in node_modules do not pass
// under Ply's settings (RFC 0013); this checks packages/sdk/**/*.d.ts alone,
// within the same program the root config builds.
import path from "node:path";
import ts from "typescript";

const root = path.resolve(import.meta.dirname, "..");
const config = ts.getParsedCommandLineOfConfigFile(path.join(root, "tsconfig.json"), {}, {
  ...ts.sys, onUnRecoverableConfigFileDiagnostic: (d) => { throw new Error(ts.flattenDiagnosticMessageText(d.messageText, "\n")); },
});
const program = ts.createProgram(config.fileNames, { ...config.options, skipLibCheck: false });
const sdk = path.join(root, "packages", "sdk") + path.sep;
const files = program.getSourceFiles().filter((file) => path.resolve(file.fileName).startsWith(sdk));
const diagnostics = files.flatMap((file) => [...program.getSyntacticDiagnostics(file), ...program.getSemanticDiagnostics(file)]);
if (diagnostics.length) {
  console.error(ts.formatDiagnostics(diagnostics, { getCanonicalFileName: (f) => f, getCurrentDirectory: () => root, getNewLine: () => "\n" }));
  process.exit(1);
}
console.log(`SDK declarations: ${files.length} files, no diagnostics`);
