// WebRTC between this page and its node for a wave app's media. The
// microphone and camera go out on two sending tracks (turning them on and
// off swaps the track, with no new offer); what the app sends comes in on
// receiving tracks, the slots, each carrying one app channel (one person)
// as the node says in `AppUpdate`s with status `slots`. Sound plays in
// <audio> elements, so the browser's echo cancellation covers it.
//
// Where WebRTC is missing or the node cannot be reached over UDP, the box
// keeps sending and receiving media on the client socket (media.ts).

import type { AppRtcResponse, AppRtcSlot } from "../../wire";

/** Receiving tracks per kind: the most people heard and seen at once. */
export const RTC_SLOTS = 8;
/** How long the connection may take before the socket is used instead. */
const CONNECT_MS = 5000;
const STATS_MS = 1000;
/** `localStorage` key; "socket" keeps app media off WebRTC (tests, troubleshooting). */
export const MEDIA_PATH_KEY = "resonator.app-media";

/** Whether to try WebRTC for app media on this page. */
export function rtcWanted(): boolean {
  if (typeof RTCPeerConnection === "undefined") {
    return false;
  }
  try {
    return localStorage.getItem(MEDIA_PATH_KEY) !== "socket";
  } catch {
    return true;
  }
}

/** Frames (or for sound, packets) so far, both ways. */
export interface RtcCounts {
  readonly soundSent: number;
  readonly videoSent: number;
  readonly soundPlayed: number;
  readonly videoShown: number;
}

export interface RtcOptions {
  /** Sends the offer to the node; resolves with its answer. */
  readonly signal: (offer: string) => Promise<AppRtcResponse>;
  /** Whether the app has the camera: a camera track. */
  readonly camera: boolean;
  /** Whether the app shows video (`camera` or `video`): video slots. */
  readonly video: boolean;
  /** Where the <audio> elements go (hidden). */
  readonly audioHost: HTMLElement;
  /** Where the video tiles go; null without video. */
  readonly videoHost: HTMLElement | null;
  readonly onCounts: (counts: RtcCounts) => void;
  /** Sound would not start without a click. */
  readonly onBlocked: (blocked: boolean) => void;
  /** The connection broke after it was up. */
  readonly onLost: () => void;
}

export class AppRtc {
  private readonly audios = new Map<string, HTMLAudioElement>();
  private readonly videos = new Map<string, HTMLVideoElement>();
  private readonly timer: ReturnType<typeof setInterval>;
  private closed = false;

  private constructor(
    private readonly pc: RTCPeerConnection,
    private readonly mic: RTCRtpSender,
    private readonly camera: RTCRtpSender | null,
    private readonly options: RtcOptions,
  ) {
    this.timer = setInterval(() => void this.count(), STATS_MS);
  }

  /** Connects; rejects when the node refuses or cannot be reached in time. */
  static async connect(options: RtcOptions): Promise<AppRtc> {
    const pc = new RTCPeerConnection({
      bundlePolicy: "max-bundle",
      rtcpMuxPolicy: "require",
    });
    const mic = pc.addTransceiver("audio", { direction: "sendonly" }).sender;
    const camera = options.camera ? pc.addTransceiver("video", { direction: "sendonly" }).sender : null;
    for (let i = 0; i < RTC_SLOTS; i++) {
      pc.addTransceiver("audio", { direction: "recvonly" });
      if (options.video) {
        pc.addTransceiver("video", { direction: "recvonly" });
      }
    }
    const rtc = new AppRtc(pc, mic, camera, options);
    pc.ontrack = (event) => rtc.track(event);
    try {
      await pc.setLocalDescription(await pc.createOffer());
      const response = await options.signal(pc.localDescription?.sdp ?? "");
      if (response.answer === undefined) {
        throw new Error(response.errorMessage ?? "The node refused WebRTC.");
      }
      await pc.setRemoteDescription({ type: "answer", sdp: response.answer });
      await connected(pc);
    } catch (error) {
      rtc.close();
      throw error;
    }
    pc.onconnectionstatechange = () => {
      if (pc.connectionState === "failed" || pc.connectionState === "closed") {
        if (!rtc.closed) {
          rtc.close();
          options.onLost();
        }
      }
    };
    return rtc;
  }

  /** Sends the microphone (or nothing). */
  setMic(track: MediaStreamTrack | null): Promise<void> {
    return this.mic.replaceTrack(track);
  }

  /** Sends the camera (or nothing). */
  setCamera(track: MediaStreamTrack | null): Promise<void> {
    return this.camera?.replaceTrack(track) ?? Promise.resolve();
  }

  /** Which slot carries which channel now: tiles show for those that do. */
  slots(slots: readonly AppRtcSlot[]): void {
    const channels = new Map(slots.map((slot) => [slot.mid, slot.channel]));
    for (const [mid, video] of this.videos) {
      const channel = channels.get(mid);
      video.hidden = channel === undefined;
      if (channel === undefined) {
        video.removeAttribute("data-channel");
      } else {
        video.setAttribute("data-channel", String(channel));
        void video.play().catch(() => {});
      }
    }
  }

  /** From a click: lets the sound start where the browser held it back. */
  unlock(): void {
    let blocked = false;
    const tries = [...this.audios.values()].map((audio) =>
      audio.play().catch(() => {
        blocked = true;
      }),
    );
    void Promise.all(tries).then(() => this.options.onBlocked(blocked));
  }

  close(): void {
    if (this.closed) {
      return;
    }
    this.closed = true;
    clearInterval(this.timer);
    this.pc.ontrack = null;
    this.pc.onconnectionstatechange = null;
    this.pc.close();
    for (const element of [...this.audios.values(), ...this.videos.values()]) {
      element.srcObject = null;
      element.remove();
    }
    this.audios.clear();
    this.videos.clear();
  }

  private track(event: RTCTrackEvent): void {
    const mid = event.transceiver.mid;
    if (mid === null || this.closed) {
      return;
    }
    const stream = new MediaStream([event.track]);
    if (event.track.kind === "audio") {
      const audio = document.createElement("audio");
      audio.autoplay = true;
      audio.hidden = true;
      audio.srcObject = stream;
      this.options.audioHost.append(audio);
      this.audios.set(mid, audio);
      void audio.play().catch(() => this.options.onBlocked(true));
      return;
    }
    if (this.options.videoHost === null) {
      return;
    }
    const video = document.createElement("video");
    video.className = "app-video-tile";
    video.autoplay = true;
    video.muted = true;
    video.playsInline = true;
    video.hidden = true;
    video.srcObject = stream;
    this.options.videoHost.append(video);
    this.videos.set(mid, video);
  }

  private async count(): Promise<void> {
    if (this.closed) {
      return;
    }
    const counts = {
      soundSent: 0,
      videoSent: 0,
      soundPlayed: 0,
      videoShown: 0,
    };
    const report = await this.pc.getStats();
    report.forEach((stat: Record<string, unknown>) => {
      const number = (key: string): number => (typeof stat[key] === "number" ? stat[key] : 0);
      if (stat["type"] === "outbound-rtp") {
        if (stat["kind"] === "video") {
          counts.videoSent += number("framesEncoded");
        } else {
          counts.soundSent += number("packetsSent");
        }
      } else if (stat["type"] === "inbound-rtp") {
        if (stat["kind"] === "video") {
          counts.videoShown += number("framesDecoded");
        } else {
          counts.soundPlayed += number("packetsReceived");
        }
      }
    });
    if (!this.closed) {
      this.options.onCounts(counts);
    }
  }
}

/** Resolves once the connection is up; rejects when it fails or takes too long. */
function connected(pc: RTCPeerConnection): Promise<void> {
  return new Promise((resolve, reject) => {
    const done = (error?: Error): void => {
      clearTimeout(timer);
      pc.removeEventListener("connectionstatechange", check);
      if (error === undefined) {
        resolve();
      } else {
        reject(error);
      }
    };
    const check = (): void => {
      if (pc.connectionState === "connected") {
        done();
      } else if (pc.connectionState === "failed" || pc.connectionState === "closed") {
        done(new Error("The WebRTC connection failed."));
      }
    };
    const timer = setTimeout(() => done(new Error("The WebRTC connection took too long.")), CONNECT_MS);
    pc.addEventListener("connectionstatechange", check);
    check();
  });
}
