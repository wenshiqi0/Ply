exports.tla = () => { try { require("tla-pkg"); return "loaded"; } catch (e) { return e.code; } };
exports.esm = () => require("esm-pkg/sub/a");
exports.sync = () => require("sync-pkg");
exports.json = () => require("./data.json");
