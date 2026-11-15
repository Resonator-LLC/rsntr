// Builds chat.wasm from chat.js with componentize-js. The WIT is passed
// inline: the splicer's sandbox cannot read files outside it.
import { componentize } from "@bytecodealliance/componentize-js";
import { readFile, writeFile } from "node:fs/promises";

const { component } = await componentize({
  sourcePath: "chat.js",
  witWorld: await readFile("../../../crates/resonator-apps/wit/app.wit", "utf8"),
  worldName: "wave-app",
  disableFeatures: ["http", "fetch-event"],
});
await writeFile("chat.wasm", component);
console.log(`chat.wasm: ${component.length} bytes`);
