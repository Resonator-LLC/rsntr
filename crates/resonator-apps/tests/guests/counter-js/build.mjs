// Builds counter_js.wasm from counter.js with componentize-js. The WIT is
// passed inline: the splicer's sandbox cannot read files outside it.
import { componentize } from "@bytecodealliance/componentize-js";
import { readFile, writeFile } from "node:fs/promises";

const { component } = await componentize({
  sourcePath: "counter.js",
  witWorld: await readFile("../../../wit/app.wit", "utf8"),
  worldName: "wave-app",
  disableFeatures: ["http", "fetch-event"],
});
await writeFile("counter_js.wasm", component);
console.log(`counter_js.wasm: ${component.length} bytes`);
