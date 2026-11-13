// Where the wave panel gets its blip views and the edit toolbar from: the
// editor module if it is there, else the plain fallback renderer. A wave
// panel can also be given a factory (tests do).

import type { ComponentType } from "preact";

import * as editor from "../editor";
import type { BlipView, BlipViewOptions } from "../editor/api";
import { createFallbackBlipView } from "./fallbackView";

export type BlipViewFactory = (options: BlipViewOptions) => BlipView;

/** The props of the editor's `EditToolbar`. */
export interface EditToolbarProps {
  view: BlipView;
  onInsertAttachment?: () => void;
  onInsertInlineReply?: () => void;
  onDone?: () => void;
}

/**
 * Renders with the editor. A blip the editor cannot render (it throws) is
 * shown as plain text instead of breaking the wave.
 */
export const defaultBlipViewFactory: BlipViewFactory = (options) => {
  try {
    return editor.createBlipView(options);
  } catch (error) {
    console.error("wave panel: the editor could not render a blip; showing plain text", error);
    return createFallbackBlipView(options);
  }
};

/** The editor's toolbar component, or null if the editor has none. */
export function editorToolbar(): ComponentType<EditToolbarProps> | null {
  const candidate = (editor as unknown as { EditToolbar?: ComponentType<EditToolbarProps> }).EditToolbar;
  return typeof candidate === "function" ? candidate : null;
}
