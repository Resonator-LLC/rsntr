// A wave app in a blip: a header we draw (name, short hash, status, Log)
// and the app's view in a closed shadow root, drawn by the patcher from
// what the app on this node renders. The app runs on the node; nothing of
// it runs here.

import { useEffect, useRef, useState } from "preact/hooks";

import type { StreamHandle, StreamHandlers } from "../../net";
import type { AppEventRequest, AppEventResponse, AppMediaRequest, AppOpenRequest, AppUpdate } from "../../wire";
import type { AppGadget } from "../editor/api";
import appUiCss from "./app-ui.css?url";
import { Mic, mediaSupported, Speaker } from "./media";
import { AppPatcher } from "./patch";

/** What an app view needs from the wave and the session. */
export interface WaveAppsUi {
  /** The serialised wave id. */
  readonly waveId: string;
  open(request: AppOpenRequest, handlers: StreamHandlers<AppUpdate>): StreamHandle;
  event(request: AppEventRequest): Promise<AppEventResponse>;
  /** Hands the app a frame of the microphone. */
  media(request: AppMediaRequest): Promise<AppEventResponse>;
  /** Allows a gadget's app for the signed-in user (`m/gadgets`, key `allowed`). */
  allow(gadgetId: string, appId: string): void;
  /** The URL of an attachment of the wave. */
  attachmentUrl(id: string): string | null;
  /** Shows the app's message log; null while there is none to show. */
  readonly showLog: ((gadgetId: string, name: string) => void) | null;
}

/** Events in the app's view that must not reach the editor around it. */
const CONTAINED = [
  "keydown",
  "keyup",
  "keypress",
  "beforeinput",
  "input",
  "paste",
  "cut",
  "copy",
  "compositionstart",
  "compositionupdate",
  "compositionend",
  "dragstart",
  "drop",
] as const;

/** How long after Allow a missing grant is taken for one still on its way. */
const GRANT_GRACE_MS = 10_000;
const REOPEN_MS = 400;

type Status = "loading" | "needs-grant" | "running" | "failed" | "stopped" | "off";

interface State {
  readonly status: Status;
  readonly name: string;
  readonly appId: string;
  readonly reason: string;
  readonly permissions: readonly string[];
}

const PERMISSION_TEXT: Readonly<Record<string, string>> = {
  wave: "post messages to everyone in this wave",
  kv: "keep data on this device",
  peers: "talk directly to the same app on other people's devices",
  streams: "open streams to other devices",
  media: "use your microphone",
};

export function shortHash(appId: string): string {
  return appId.startsWith("sha256:") ? appId.slice(7, 15) : appId.slice(0, 8);
}

export function AppView({ gadget, apps }: { readonly gadget: AppGadget; readonly apps: WaveAppsUi }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const shadowRef = useRef<{ root: ShadowRoot; patcher: AppPatcher } | null>(null);
  const allowedAt = useRef(0);
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<State>({
    status: "loading",
    name: "",
    appId: gadget.app,
    reason: "",
    permissions: [],
  });
  const nameRef = useRef("");
  const boxRef = useRef<HTMLDivElement>(null);
  const speakerRef = useRef<Speaker | null>(null);
  const micRef = useRef<Mic | null>(null);
  const [mic, setMic] = useState<"off" | "starting" | "on">("off");
  const [micError, setMicError] = useState("");

  const stopMic = (): void => {
    micRef.current?.stop();
    micRef.current = null;
    setMic("off");
  };
  // The microphone and the speaker end with the view.
  useEffect(
    () => () => {
      micRef.current?.stop();
      micRef.current = null;
      speakerRef.current?.stop();
      speakerRef.current = null;
    },
    [],
  );

  // The shadow root, once.
  useEffect(() => {
    const host = hostRef.current;
    if (host === null || shadowRef.current !== null) {
      return;
    }
    const root = host.attachShadow({ mode: "closed" });
    const link = document.createElement("link");
    link.setAttribute("rel", "stylesheet");
    link.setAttribute("href", appUiCss);
    link.setAttribute("data-app-fixed", "");
    root.appendChild(link);
    const patcher = new AppPatcher(root, {
      send: (event) => {
        void apps
          .event({
            waveId: apps.waveId,
            gadgetId: gadget.id,
            node: event.node,
            name: event.name,
            ...(event.value !== undefined ? { value: event.value } : {}),
            fields: event.fields ?? [],
          })
          .catch(() => {});
      },
      attachmentUrl: (id) => apps.attachmentUrl(id),
      get appName() {
        return nameRef.current;
      },
    });
    shadowRef.current = { root, patcher };
    const stop = (event: Event) => event.stopPropagation();
    for (const type of CONTAINED) {
      host.addEventListener(type, stop);
    }
  }, []);

  // The stream of the app's state and renders.
  useEffect(() => {
    let handle: StreamHandle | null = null;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let ended = false;
    const reopen = (): void => {
      timer = setTimeout(() => setAttempt((n) => n + 1), REOPEN_MS);
    };
    try {
      handle = apps.open(
        { waveId: apps.waveId, gadgetId: gadget.id },
        {
          onMessage: (update) => {
            if (update.status === "media") {
              speakerRef.current ??= new Speaker((played) =>
                boxRef.current?.setAttribute("data-media-played", String(played)),
              );
              speakerRef.current.play(update.media);
              return;
            }
            const status = update.status as Status;
            if (status === "needs-grant" && Date.now() - allowedAt.current < GRANT_GRACE_MS) {
              // The grant is on its way to the server.
              reopen();
              return;
            }
            if (update.name !== undefined) {
              nameRef.current = update.name;
            }
            setState((old) => ({
              status,
              name: update.name ?? old.name,
              appId: update.appId ?? old.appId,
              reason: update.reason ?? "",
              permissions: update.permissions.length > 0 ? update.permissions : old.permissions,
            }));
            if (status === "running") {
              shadowRef.current?.patcher.apply(update.nodes);
            }
          },
          onFinished: (error) => {
            ended = true;
            stopMic();
            if (error !== undefined) {
              setState((old) => ({ ...old, status: /apps are off/i.test(error) ? "off" : "failed", reason: error }));
            }
          },
          onDisconnected: () => {
            ended = true;
            stopMic();
            setState((old) => ({ ...old, status: "stopped", reason: "Disconnected" }));
          },
        },
      );
    } catch (error) {
      setState((old) => ({ ...old, status: "stopped", reason: error instanceof Error ? error.message : String(error) }));
    }
    return () => {
      if (timer !== null) {
        clearTimeout(timer);
      }
      if (!ended) {
        handle?.cancel();
      }
    };
  }, [gadget.id, gadget.app, attempt]);

  const allow = (): void => {
    allowedAt.current = Date.now();
    apps.allow(gadget.id, state.appId);
    setState((old) => ({ ...old, status: "loading" }));
    setAttempt((n) => n + 1);
  };
  const toggleMic = (): void => {
    if (mic !== "off") {
      stopMic();
      return;
    }
    setMic("starting");
    setMicError("");
    let sent = 0;
    Mic.start((data) => {
      void apps.media({ waveId: apps.waveId, gadgetId: gadget.id, data }).catch(() => {});
      sent += 1;
      boxRef.current?.setAttribute("data-media-sent", String(sent));
    }).then(
      (started) => {
        micRef.current = started;
        setMic("on");
      },
      (error: unknown) => {
        setMic("off");
        setMicError(error instanceof Error ? error.message : String(error));
      },
    );
  };
  const restart = (): void => {
    setState((old) => ({ ...old, status: "loading", reason: "" }));
    setAttempt((n) => n + 1);
  };

  const name = state.name !== "" ? state.name : "App";
  const statusText: Record<Status, string> = {
    loading: "Starting",
    "needs-grant": "Not allowed yet",
    running: "Running",
    failed: "Failed",
    stopped: "Stopped",
    off: "Apps are off",
  };

  return (
    <div ref={boxRef} class={`app-box app-${state.status}`} data-gadget={gadget.id}>
      <div class="app-header">
        <span class="app-name">{name}</span>
        <span class="app-hash" title={state.appId}>
          {shortHash(state.appId)}
        </span>
        <span class="app-status" role="status">
          {statusText[state.status]}
        </span>
        {state.status === "running" && state.permissions.includes("media") ? (
          <button
            type="button"
            class={`app-mic-button${mic === "on" ? " app-mic-on" : ""}`}
            aria-pressed={mic === "on"}
            disabled={!mediaSupported() || mic === "starting"}
            title={
              !mediaSupported()
                ? "This browser cannot record sound here (it needs a secure page and WebCodecs)"
                : micError !== ""
                  ? micError
                  : `Lets ${name} hear your microphone while on`
            }
            onClick={toggleMic}
          >
            {mic === "on" ? "Mic on" : "Mic off"}
          </button>
        ) : null}
        {apps.showLog !== null ? (
          <button type="button" class="app-log-button" onClick={() => apps.showLog?.(gadget.id, name)}>
            Log
          </button>
        ) : null}
      </div>
      {state.status === "needs-grant" ? (
        <div class="app-grant">
          <p>
            <strong>{name}</strong> wants to run on your device. It can:
          </p>
          <ul>
            {state.permissions.length === 0 ? <li>draw in this box</li> : null}
            {state.permissions.map((p) => (
              <li key={p}>{PERMISSION_TEXT[p] ?? p}</li>
            ))}
          </ul>
          <button type="button" class="app-allow" onClick={allow}>
            Allow
          </button>
        </div>
      ) : null}
      {state.status === "failed" || state.status === "stopped" ? (
        <div class="app-ended">
          {state.reason !== "" ? <span class="app-reason">{state.reason}</span> : null}
          <button type="button" class="app-restart" onClick={restart}>
            Start again
          </button>
        </div>
      ) : null}
      <div ref={hostRef} class="app-surface" hidden={state.status !== "running"} />
    </div>
  );
}
