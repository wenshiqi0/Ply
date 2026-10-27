// ply-divergence: Ply does not load native addons, so it resolves like `node --no-addons` and never matches the "node-addons" condition.
import addon from "addon-pkg";
console.log(JSON.stringify({ addon }));
