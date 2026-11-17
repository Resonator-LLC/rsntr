// A wave app in a blip: a header we draw (name, short hash, status, Log)
// and the app's view in a closed shadow root, drawn by the patcher from
// what the app on this node renders. The app runs on the node; nothing of
// it runs here.

import { useEffect, useRef, useState } from "preact/hooks";

import type { StreamHandle, StreamHandlers } from "../../net";
import type {
  AppEventRequest,
  AppEventResponse,
  AppMediaRequest,
  AppOpenRequest,
  AppRtcRequest,
  AppRtcResponse,
  AppUpdate,
} from "../../wire";
import type { AppGadget } from "../editor/api";
import appUiCss from "./app-ui.css?url";
import {
  Camera,
  CAMERA_CONSTRAINTS,
  cameraFailure,
  cameraUnavailable,
  Mic,
  MIC_CONSTRAINTS,
  micFailure,
  micUnavailable,
  Screen,
  Speaker,
} from "./media";
import { AppPatcher } from "./patch";
import { AppRtc, type RtcCounts, rtcWanted } from "./rtc";

/** What an app view needs from the wave and the session. */
export interface WaveAppsUi {
  /** The serialised wave id. */
  readonly waveId: string;
  open(request: AppOpenRequest, handlers: StreamHandlers<AppUpdate>): StreamHandle;
  event(request: AppEventRequest): Promise<AppEventResponse>;
  /** Hands the app a frame of the microphone (or, with `video`, the camera). */
  media(request: AppMediaRequest): Promise<AppEventResponse>;
  /** Offers the app a WebRTC connection for its media; missing where there is none. */
  readonly rtc?: (request: AppRtcRequest) => Promise<AppRtcResponse>;
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
/**
 * When this page allowed an app, by gadget: a box opened just after its
 * app was inserted (and allowed) may reach the node before the grant does.
 */
const allowedGadgets = new Map<string, number>();

/** Notes that this page allowed the app of `gadgetId` (see `GRANT_GRACE_MS`). */
export function noteAllowed(gadgetId: string): void {
  allowedGadgets.set(gadgetId, Date.now());
}
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
  media: "hear your microphone while you turn it on, and play sound",
  camera: "see your camera while you turn it on, and show video",
  video: "show video",
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
  const [soundBlocked, setSoundBlocked] = useState(false);
  const cameraRef = useRef<Camera | null>(null);
  const screenRef = useRef<Screen | null>(null);
  const tilesRef = useRef<HTMLDivElement>(null);
  const previewRef = useRef<HTMLVideoElement>(null);
  const [camera, setCamera] = useState<"off" | "starting" | "on">("off");
  const [cameraError, setCameraError] = useState("");
  // Media over WebRTC while connected; the socket before and otherwise.
  const rtcRef = useRef<AppRtc | null>(null);
  const [rtcState, setRtcState] = useState<"socket" | "connecting" | "rtc">("socket");
  /** The microphone and camera as tracks, while sent over WebRTC. */
  const micStreamRef = useRef<MediaStream | null>(null);
  const cameraStreamRef = useRef<MediaStream | null>(null);

  const screen = (): Screen | null => {
    if (screenRef.current === null && tilesRef.current !== null) {
      screenRef.current = new Screen(tilesRef.current, (shown) =>
        boxRef.current?.setAttribute("data-video-shown", String(shown)),
      );
    }
    return screenRef.current;
  };
  const stopCamera = (): void => {
    cameraRef.current?.stop();
    cameraRef.current = null;
    stopTracks(cameraStreamRef.current);
    cameraStreamRef.current = null;
    void rtcRef.current?.setCamera(null).catch(() => {});
    if (previewRef.current !== null) {
      previewRef.current.srcObject = null;
    }
    setCamera("off");
  };

  const speaker = (): Speaker =>
    (speakerRef.current ??= new Speaker(
      (played) => boxRef.current?.setAttribute("data-media-played", String(played)),
      setSoundBlocked,
    ));

  const stopMic = (): void => {
    micRef.current?.stop();
    micRef.current = null;
    stopTracks(micStreamRef.current);
    micStreamRef.current = null;
    void rtcRef.current?.setMic(null).catch(() => {});
    setMic("off");
  };
  const closeRtc = (): void => {
    rtcRef.current?.close();
    rtcRef.current = null;
    setRtcState("socket");
  };
  const showCounts = (counts: RtcCounts): void => {
    const box = boxRef.current;
    if (box === null) {
      return;
    }
    const set = (name: string, value: number): void => {
      if (value > 0) {
        box.setAttribute(name, String(value));
      }
    };
    set("data-media-sent", counts.soundSent);
    set("data-video-sent", counts.videoSent);
    set("data-media-played", counts.soundPlayed);
    set("data-video-shown", counts.videoShown);
  };
  // The microphone and the speaker end with the view.
  useEffect(
    () => () => {
      micRef.current?.stop();
      micRef.current = null;
      speakerRef.current?.stop();
      speakerRef.current = null;
      cameraRef.current?.stop();
      cameraRef.current = null;
      screenRef.current?.stop();
      screenRef.current = null;
      stopTracks(micStreamRef.current);
      stopTracks(cameraStreamRef.current);
      rtcRef.current?.close();
      rtcRef.current = null;
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
            if (update.status === "slots") {
              rtcRef.current?.slots(update.slots);
              return;
            }
            if (update.status === "media") {
              if (update.media.length > 0) {
                speaker().play(update.media);
              }
              if (update.video.length > 0) {
                screen()?.play(update.video);
              }
              return;
            }
            const status = update.status as Status;
            const allowed = Math.max(allowedAt.current, allowedGadgets.get(gadget.id) ?? 0);
            if (status === "needs-grant" && Date.now() - allowed < GRANT_GRACE_MS) {
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
            stopCamera();
            closeRtc();
            if (error !== undefined) {
              setState((old) => ({ ...old, status: /apps are off/i.test(error) ? "off" : "failed", reason: error }));
            }
          },
          onDisconnected: () => {
            ended = true;
            stopMic();
            stopCamera();
            closeRtc();
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

  const hasMedia = state.permissions.includes("media");
  const hasCamera = state.permissions.includes("camera");
  // Shows the app's video; only `camera` also has the person's own.
  const hasVideo = hasCamera || state.permissions.includes("video");

  // A WebRTC connection for a running app with media, where it can be had.
  useEffect(() => {
    const signal = apps.rtc;
    const audioHost = boxRef.current;
    if (
      state.status !== "running" ||
      !(hasMedia || hasVideo) ||
      signal === undefined ||
      audioHost === null ||
      rtcRef.current !== null ||
      !rtcWanted()
    ) {
      return;
    }
    let cancelled = false;
    setRtcState("connecting");
    AppRtc.connect({
      signal: (offer) => signal({ waveId: apps.waveId, gadgetId: gadget.id, offer }),
      camera: hasCamera,
      video: hasVideo,
      audioHost,
      videoHost: tilesRef.current,
      onCounts: showCounts,
      onBlocked: setSoundBlocked,
      onLost: () => {
        rtcRef.current = null;
        setRtcState("socket");
        // Their tracks went over WebRTC; turned on again, they use the socket.
        stopMic();
        stopCamera();
      },
    }).then(
      (rtc) => {
        if (cancelled) {
          rtc.close();
          return;
        }
        rtcRef.current = rtc;
        setRtcState("rtc");
        // What was turned on before the connection goes over it too.
        const micTrack = micStreamRef.current?.getAudioTracks()[0];
        if (micTrack !== undefined) {
          void rtc.setMic(micTrack).catch(() => {});
        }
        const cameraTrack = cameraStreamRef.current?.getVideoTracks()[0];
        if (cameraTrack !== undefined) {
          void rtc.setCamera(cameraTrack).catch(() => {});
        }
      },
      () => {
        if (!cancelled) {
          setRtcState("socket");
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [state.status, hasMedia, hasCamera, hasVideo, attempt]);

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
    const rtc = rtcRef.current;
    const unavailable = micUnavailable(rtc === null);
    if (unavailable !== null) {
      setMicError(unavailable);
      return;
    }
    // The click also lets what the app sends back be heard.
    speaker().unlock();
    rtc?.unlock();
    setMic("starting");
    setMicError("");
    if (rtc !== null) {
      navigator.mediaDevices.getUserMedia({ audio: MIC_CONSTRAINTS }).then(
        (stream) => {
          micStreamRef.current = stream;
          void rtcRef.current?.setMic(stream.getAudioTracks()[0] ?? null).catch(() => {});
          setMic("on");
        },
        (error: unknown) => {
          setMic("off");
          setMicError(micFailure(error));
        },
      );
      return;
    }
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
        setMicError(micFailure(error));
      },
    );
  };
  const toggleCamera = (): void => {
    if (camera !== "off") {
      stopCamera();
      return;
    }
    const rtc = rtcRef.current;
    const unavailable = cameraUnavailable(rtc === null);
    if (unavailable !== null) {
      setCameraError(unavailable);
      return;
    }
    speaker().unlock();
    rtc?.unlock();
    setCamera("starting");
    setCameraError("");
    if (rtc !== null) {
      navigator.mediaDevices.getUserMedia({ video: CAMERA_CONSTRAINTS }).then(
        (stream) => {
          cameraStreamRef.current = stream;
          void rtcRef.current?.setCamera(stream.getVideoTracks()[0] ?? null).catch(() => {});
          if (previewRef.current !== null) {
            previewRef.current.srcObject = stream;
            void previewRef.current.play().catch(() => {});
          }
          setCamera("on");
        },
        (error: unknown) => {
          setCamera("off");
          setCameraError(cameraFailure(error));
        },
      );
      return;
    }
    let sent = 0;
    Camera.start((data) => {
      void apps.media({ waveId: apps.waveId, gadgetId: gadget.id, data, video: true }).catch(() => {});
      sent += 1;
      boxRef.current?.setAttribute("data-video-sent", String(sent));
    }).then(
      (started) => {
        cameraRef.current = started;
        if (previewRef.current !== null) {
          previewRef.current.srcObject = started.stream;
          void previewRef.current.play().catch(() => {});
        }
        setCamera("on");
      },
      (error: unknown) => {
        setCamera("off");
        setCameraError(cameraFailure(error));
      },
    );
  };
  const restart = (): void => {
    setState((old) => ({ ...old, status: "loading", reason: "" }));
    setAttempt((n) => n + 1);
  };

  const name = state.name !== "" ? state.name : "App";
  const mediaError = micError !== "" ? micError : cameraError;
  const using = [mic === "on" ? "your microphone" : null, camera === "on" ? "your camera" : null].filter(
    (part): part is string => part !== null,
  );
  const mediaNote =
    using.length > 0
      ? `${name} gets ${using.join(" and ")} until you turn ${using.length > 1 ? "them" : "it"} off.`
      : hasMedia && hasCamera
        ? `${name} gets nothing from you until you turn the microphone or camera on.`
        : hasCamera
          ? `${name} sees nothing until you turn the camera on.`
          : `${name} hears nothing until you turn the microphone on.`;
  const statusText: Record<Status, string> = {
    loading: "Starting",
    "needs-grant": "Not allowed yet",
    running: "Running",
    failed: "Failed",
    stopped: "Stopped",
    off: "Apps are off",
  };

  return (
    <div
      ref={boxRef}
      class={`app-box app-${state.status}`}
      data-gadget={gadget.id}
      data-media-path={hasMedia || hasVideo ? rtcState : undefined}
    >
      <div class="app-header">
        <span class="app-name">{name}</span>
        <span class="app-hash" title={state.appId}>
          {shortHash(state.appId)}
        </span>
        <span class="app-status" role="status">
          {statusText[state.status]}
        </span>
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
      {state.status === "running" && (hasMedia || hasCamera) ? (
        <div class="app-media">
          {hasMedia ? (
            <button
              type="button"
              class={`app-mic-button${mic === "on" ? " app-mic-on" : ""}`}
              aria-pressed={mic === "on"}
              disabled={mic === "starting"}
              onClick={toggleMic}
            >
              {mic === "on" ? "Turn microphone off" : mic === "starting" ? "Starting microphone" : "Turn microphone on"}
            </button>
          ) : null}
          {hasCamera ? (
            <button
              type="button"
              class={`app-camera-button${camera === "on" ? " app-mic-on" : ""}`}
              aria-pressed={camera === "on"}
              disabled={camera === "starting"}
              onClick={toggleCamera}
            >
              {camera === "on" ? "Turn camera off" : camera === "starting" ? "Starting camera" : "Turn camera on"}
            </button>
          ) : null}
          {soundBlocked ? (
            <button
              type="button"
              class="app-sound-button"
              onClick={() => {
                speakerRef.current?.unlock();
                rtcRef.current?.unlock();
              }}
            >
              Play sound
            </button>
          ) : null}
          <span class={mediaError !== "" ? "app-media-note app-media-error" : "app-media-note"} role="status">
            {mediaError !== "" ? mediaError : mediaNote}
          </span>
        </div>
      ) : null}
      {hasVideo ? (
        <div class="app-video" hidden={state.status !== "running"}>
          {hasCamera ? (
            <video ref={previewRef} class="app-video-self" muted playsInline aria-label="You" hidden={camera !== "on"} />
          ) : null}
          <div ref={tilesRef} class="app-video-tiles" />
        </div>
      ) : null}
      <div ref={hostRef} class="app-surface" hidden={state.status !== "running"} />
    </div>
  );
}

function stopTracks(stream: MediaStream | null): void {
  for (const track of stream?.getTracks() ?? []) {
    track.stop();
  }
}
