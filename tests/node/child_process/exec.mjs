// Running commands: execFileSync, execSync, execFile, exec and spawnSync.
import cp from "node:child_process";
const show = (label, value) => console.log(label, JSON.stringify(value));
show("execFileSync", [cp.execFileSync("/bin/echo", ["hi", "there"], { encoding: "utf8" }), [...cp.execFileSync("/bin/echo", ["x"])]]);
show("execSync", cp.execSync("echo one; echo two", { encoding: "utf8" }));
try { cp.execSync("exit 3", { stdio: "pipe" }); } catch (e) { show("execSync failure", { status: e.status, error: e instanceof Error }); }
show("execFile", await new Promise((resolve) => cp.execFile("/bin/echo", ["cb"], (error, stdout, stderr) => resolve([error, stdout, stderr]))));
show("exec failure", await new Promise((resolve) => cp.exec("echo out; echo err 1>&2; exit 2", (error, stdout, stderr) => resolve([error.code, stdout, stderr]))));
const r = cp.spawnSync("/bin/sh", ["-c", "echo spawned; exit 4"], { encoding: "utf8" });
show("spawnSync", { status: r.status, stdout: r.stdout, signal: r.signal });
