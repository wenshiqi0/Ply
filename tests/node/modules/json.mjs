// JSON modules need the import attribute in ES modules, not in require().
import data from "./data.json" with { type: "json" };
import pkgData from "esm-pkg/data.json" with { type: "json" };
const show = (label, value) => console.log(label, JSON.stringify(value));
show("static", { data, pkgData });
show("dynamic", (await import("./data.json", { with: { type: "json" } })).default);
for (const [spec, options] of [["./data.json", undefined], ["./modern.js", { with: { type: "json" } }]]) {
  try { await import(spec, options); show(spec, "loaded"); } catch (e) { show(spec, e.code); }
}
