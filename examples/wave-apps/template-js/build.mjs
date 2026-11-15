// Builds my-app.wasm from my-app.js with componentize-js. The WIT is passed
// inline: the splicer's sandbox cannot read files outside it.
import { componentize } from "@bytecodealliance/componentize-js";
import { readFile, writeFile } from "node:fs/promises";

const { component } = await componentize({
  sourcePath: "my-app.js",
  witWorld: await readFile("../../../crates/resonator-apps/wit/app.wit", "utf8"),
  worldName: "wave-app",
  disableFeatures: ["http", "fetch-event"],
});
await writeFile("my-app.wasm", component);
console.log(`my-app.wasm: ${component.length} bytes`);
