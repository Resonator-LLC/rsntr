// The blip editor module. See `api.ts` for the contract.
export * from "./api";

import type { BlipView, BlipViewOptions } from "./api";
import { EditorView } from "./view";

export { EditToolbar } from "./Toolbar";
export type { EditToolbarProps } from "./Toolbar";

/** A live rendering of a blip document, read-only until `setEditable(true)`. */
export function createBlipView(options: BlipViewOptions): BlipView {
  return new EditorView(options);
}
