// node:os values that do not depend on the machine.
import os from "node:os";
const show = (label, value) => console.log(label, JSON.stringify(value));
show("EOL", os.EOL);
show("platform", { platform: os.platform() === process.platform, type: os.type() === (process.platform === "darwin" ? "Darwin" : "Linux"), arch: os.arch() === process.arch });
show("tmpdir", os.tmpdir());
show("shapes", { cpus: Array.isArray(os.cpus()), totalmem: typeof os.totalmem(), freemem: typeof os.freemem(), hostname: typeof os.hostname(), release: typeof os.release() });
