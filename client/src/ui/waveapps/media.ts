// Sound and video for wave apps with the `media` and `camera`
// permissions. The microphone and camera are captured here, encoded with
// WebCodecs (Opus; VP8, or H.264 where there is no VP8) and sent to the
// app one frame at a time. What the app sends back is decoded per channel
// (one per person) and played or drawn. The app on the node never touches
// the devices; the person turns them on and off in the bar we draw.

const SAMPLE_RATE = 48_000;
/** Samples per block handed to the encoder. */
const BLOCK = 1024;
/** How far ahead of now playback starts, to ride out jitter. */
const LEAD_SECONDS = 0.08;

/** Why the microphone cannot be used here, or null if it can be tried. */
export function micUnavailable(): string | null {
  if (typeof window !== "undefined" && !window.isSecureContext) {
    return "The browser allows the microphone only on a secure page: open this page over HTTPS or as localhost.";
  }
  if (typeof navigator === "undefined" || navigator.mediaDevices?.getUserMedia === undefined) {
    return "This browser cannot use a microphone here.";
  }
  if (typeof AudioEncoder === "undefined" || typeof AudioDecoder === "undefined" || typeof AudioContext === "undefined") {
    return "This browser cannot encode sound (it has no WebCodecs audio).";
  }
  return null;
}

/** A sentence for a failure to start the microphone. */
export function micFailure(error: unknown): string {
  const name = error instanceof DOMException || error instanceof Error ? error.name : "";
  switch (name) {
    case "NotAllowedError":
    case "SecurityError":
      return "The microphone is blocked for this page. Allow it in the browser (the icon in the address bar, or the site settings) and try again.";
    case "NotFoundError":
    case "OverconstrainedError":
      return "No microphone was found.";
    case "NotReadableError":
    case "AbortError":
      return "The microphone is in use by another program, or the system does not allow the browser to use it.";
    default:
      return error instanceof Error ? error.message : String(error);
  }
}

function base64(bytes: Uint8Array): string {
  let text = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    text += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(text);
}

function unbase64(text: string): Uint8Array {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/** The microphone, encoded: each Opus frame goes to `send` as base64. */
export class Mic {
  private constructor(
    private readonly stream: MediaStream,
    private readonly context: AudioContext,
    private readonly encoder: AudioEncoder,
    private readonly nodes: readonly AudioNode[],
  ) {}

  /** Starts the microphone; call it from a click, so the browser asks and lets sound run. */
  static async start(send: (frame: string) => void): Promise<Mic> {
    const config: AudioEncoderConfig = { codec: "opus", sampleRate: SAMPLE_RATE, numberOfChannels: 1, bitrate: 24_000 };
    if (!(await AudioEncoder.isConfigSupported(config)).supported) {
      throw new Error("This browser cannot encode Opus.");
    }
    // Made before the first await of the prompt, while the click counts.
    const context = new AudioContext({ sampleRate: SAMPLE_RATE });
    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, channelCount: 1 },
      });
      await context.resume();
    } catch (error) {
      void context.close();
      throw error;
    }
    const encoder = new AudioEncoder({
      output: (chunk) => {
        const bytes = new Uint8Array(chunk.byteLength);
        chunk.copyTo(bytes);
        send(base64(bytes));
      },
      error: () => {},
    });
    encoder.configure(config);
    const source = context.createMediaStreamSource(stream);
    // ScriptProcessor works everywhere WebCodecs does, with no module to load.
    const processor = context.createScriptProcessor(BLOCK, 1, 1);
    let timestamp = 0;
    processor.onaudioprocess = (event) => {
      const samples = new Float32Array(event.inputBuffer.getChannelData(0));
      if (encoder.state !== "configured") {
        return;
      }
      const data = new AudioData({
        format: "f32-planar",
        sampleRate: SAMPLE_RATE,
        numberOfFrames: samples.length,
        numberOfChannels: 1,
        timestamp,
        data: samples,
      });
      timestamp += Math.round((samples.length / SAMPLE_RATE) * 1_000_000);
      encoder.encode(data);
      data.close();
    };
    // The processor runs only while connected; its output is silence.
    const mute = context.createGain();
    mute.gain.value = 0;
    source.connect(processor);
    processor.connect(mute);
    mute.connect(context.destination);
    return new Mic(stream, context, encoder, [source, processor, mute]);
  }

  stop(): void {
    for (const node of this.nodes) {
      node.disconnect();
    }
    for (const track of this.stream.getTracks()) {
      track.stop();
    }
    if (this.encoder.state !== "closed") {
      this.encoder.close();
    }
    void this.context.close();
  }
}

/** Plays what an app sends: one Opus decoder per channel. */
export class Speaker {
  private context: AudioContext | null = null;
  private readonly decoders = new Map<number, { decoder: AudioDecoder; next: number; timestamp: number }>();
  /** Frames played. */
  played = 0;

  /**
   * `onPlayed` is told the number of frames played so far; `onBlocked`
   * whether the browser holds the sound back until the person clicks.
   */
  constructor(
    private readonly onPlayed: (played: number) => void = () => {},
    private readonly onBlocked: (blocked: boolean) => void = () => {},
  ) {}

  /** Lets sound play; call it from a click. */
  unlock(): void {
    if (typeof AudioContext === "undefined") {
      return;
    }
    this.context ??= new AudioContext({ sampleRate: SAMPLE_RATE });
    void this.context.resume().then(() => this.onBlocked(false));
  }

  /** Plays frames from an `AppUpdate`'s `media`. */
  play(frames: readonly string[]): void {
    if (typeof AudioDecoder === "undefined") {
      return;
    }
    for (const frame of frames) {
      const bytes = unbase64(frame);
      if (bytes.length < 2) {
        continue;
      }
      const channel = this.channel(bytes[0]!);
      channel.decoder.decode(
        new EncodedAudioChunk({ type: "key", timestamp: channel.timestamp, data: bytes.subarray(1) }),
      );
      channel.timestamp += 20_000;
    }
  }

  private channel(number: number) {
    const known = this.decoders.get(number);
    if (known !== undefined) {
      return known;
    }
    if (this.context === null) {
      this.context = new AudioContext({ sampleRate: SAMPLE_RATE });
    }
    const context = this.context;
    const state = { decoder: null as unknown as AudioDecoder, next: 0, timestamp: 0 };
    state.decoder = new AudioDecoder({
      output: (data) => {
        if (context.state !== "running") {
          // Held back until a click; old sound is not worth keeping.
          data.close();
          this.onBlocked(true);
          return;
        }
        const samples = new Float32Array(data.numberOfFrames);
        data.copyTo(samples, { planeIndex: 0, format: "f32-planar" });
        const buffer = context.createBuffer(1, samples.length, data.sampleRate);
        buffer.copyToChannel(samples, 0);
        data.close();
        const source = context.createBufferSource();
        source.buffer = buffer;
        source.connect(context.destination);
        const at = Math.max(context.currentTime + LEAD_SECONDS, state.next);
        source.start(at);
        state.next = at + buffer.duration;
        this.played += 1;
        this.onPlayed(this.played);
      },
      error: () => {
        this.decoders.delete(number);
      },
    });
    state.decoder.configure({ codec: "opus", sampleRate: SAMPLE_RATE, numberOfChannels: 1 });
    this.decoders.set(number, state);
    return state;
  }

  stop(): void {
    for (const { decoder } of this.decoders.values()) {
      if (decoder.state !== "closed") {
        decoder.close();
      }
    }
    this.decoders.clear();
    void this.context?.close();
    this.context = null;
  }
}

const VIDEO_WIDTH = 320;
const VIDEO_HEIGHT = 240;
const VIDEO_FPS = 15;
/** A key frame this often, so that someone who joins sees a picture soon. */
const KEY_EVERY = 2 * VIDEO_FPS;
/** A person's picture goes away after this long without a frame. */
const TILE_IDLE_MS = 3000;

/** Video codecs by the number in a frame's flags (bits 1-3). */
const VIDEO_CODECS: readonly { readonly id: number; readonly codec: string; readonly avc?: { format: "annexb" } }[] = [
  { id: 0, codec: "vp8" },
  { id: 1, codec: "avc1.42E01F", avc: { format: "annexb" } },
];

/** Why the camera cannot be used here, or null if it can be tried. */
export function cameraUnavailable(): string | null {
  if (typeof window !== "undefined" && !window.isSecureContext) {
    return "The browser allows the camera only on a secure page: open this page over HTTPS or as localhost.";
  }
  if (typeof navigator === "undefined" || navigator.mediaDevices?.getUserMedia === undefined) {
    return "This browser cannot use a camera here.";
  }
  if (typeof VideoEncoder === "undefined" || typeof VideoDecoder === "undefined" || typeof VideoFrame === "undefined") {
    return "This browser cannot encode video (it has no WebCodecs video).";
  }
  return null;
}

/** A sentence for a failure to start the camera. */
export function cameraFailure(error: unknown): string {
  return micFailure(error).replace(/microphone/g, "camera");
}

/**
 * The camera, encoded: each frame goes to `send` as base64 of a flags byte
 * (bit 0 a key frame, bits 1-3 the codec) and the encoded frame.
 */
export class Camera {
  private constructor(
    /** For the person's own picture. */
    readonly stream: MediaStream,
    private readonly video: HTMLVideoElement,
    private readonly encoder: VideoEncoder,
    private readonly timer: ReturnType<typeof setInterval>,
  ) {}

  /** Starts the camera; call it from a click, so the browser asks. */
  static async start(send: (frame: string) => void): Promise<Camera> {
    let chosen: (typeof VIDEO_CODECS)[number] | null = null;
    let config: VideoEncoderConfig | null = null;
    for (const candidate of VIDEO_CODECS) {
      const tried: VideoEncoderConfig = {
        codec: candidate.codec,
        width: VIDEO_WIDTH,
        height: VIDEO_HEIGHT,
        bitrate: 300_000,
        framerate: VIDEO_FPS,
        latencyMode: "realtime",
        ...(candidate.avc !== undefined ? { avc: candidate.avc } : {}),
      };
      if ((await VideoEncoder.isConfigSupported(tried)).supported === true) {
        chosen = candidate;
        config = tried;
        break;
      }
    }
    if (chosen === null || config === null) {
      throw new Error("This browser cannot encode VP8 or H.264 video.");
    }
    const stream = await navigator.mediaDevices.getUserMedia({
      video: { width: { ideal: VIDEO_WIDTH }, height: { ideal: VIDEO_HEIGHT }, frameRate: { ideal: VIDEO_FPS } },
    });
    const video = document.createElement("video");
    video.muted = true;
    video.playsInline = true;
    video.srcObject = stream;
    try {
      await video.play();
    } catch (error) {
      for (const track of stream.getTracks()) {
        track.stop();
      }
      throw error;
    }
    const codecBits = chosen.id << 1;
    const encoder = new VideoEncoder({
      output: (chunk) => {
        const bytes = new Uint8Array(chunk.byteLength + 1);
        bytes[0] = (chunk.type === "key" ? 1 : 0) | codecBits;
        chunk.copyTo(bytes.subarray(1));
        send(base64(bytes));
      },
      error: () => {},
    });
    encoder.configure(config);
    const canvas = document.createElement("canvas");
    canvas.width = VIDEO_WIDTH;
    canvas.height = VIDEO_HEIGHT;
    const context = canvas.getContext("2d");
    let count = 0;
    const timer = setInterval(() => {
      // A slow encoder skips frames rather than falling behind.
      if (context === null || encoder.state !== "configured" || encoder.encodeQueueSize > 2 || video.readyState < 2) {
        return;
      }
      context.drawImage(video, 0, 0, VIDEO_WIDTH, VIDEO_HEIGHT);
      const frame = new VideoFrame(canvas, { timestamp: Math.round(performance.now() * 1000) });
      encoder.encode(frame, { keyFrame: count % KEY_EVERY === 0 });
      frame.close();
      count += 1;
    }, 1000 / VIDEO_FPS);
    return new Camera(stream, video, encoder, timer);
  }

  stop(): void {
    clearInterval(this.timer);
    if (this.encoder.state !== "closed") {
      this.encoder.close();
    }
    for (const track of this.stream.getTracks()) {
      track.stop();
    }
    this.video.srcObject = null;
  }
}

interface Tile {
  readonly canvas: HTMLCanvasElement;
  decoder: VideoDecoder | null;
  codec: number;
  /** Delta frames are useless until a key frame came. */
  waitingForKey: boolean;
  timestamp: number;
  last: number;
}

/** Shows what an app sends: one decoder and one picture per channel. */
export class Screen {
  private readonly tiles = new Map<number, Tile>();
  private readonly sweep: ReturnType<typeof setInterval>;
  /** Frames shown. */
  shown = 0;

  /** Pictures go into `host`; `onShown` is told the number of frames shown so far. */
  constructor(
    private readonly host: HTMLElement,
    private readonly onShown: (shown: number) => void = () => {},
  ) {
    this.sweep = setInterval(() => {
      const now = Date.now();
      for (const [channel, tile] of this.tiles) {
        if (now - tile.last > TILE_IDLE_MS) {
          this.drop(channel);
        }
      }
    }, 1000);
  }

  /** Shows frames from an `AppUpdate`'s `video`. */
  play(frames: readonly string[]): void {
    if (typeof VideoDecoder === "undefined") {
      return;
    }
    for (const frame of frames) {
      const bytes = unbase64(frame);
      if (bytes.length < 3) {
        continue;
      }
      const channel = bytes[0]!;
      const flags = bytes[1]!;
      const key = (flags & 1) === 1;
      const codec = (flags >> 1) & 7;
      const tile = this.tile(channel);
      tile.last = Date.now();
      if (tile.decoder === null || tile.codec !== codec) {
        if (!key) {
          continue;
        }
        this.configure(tile, codec);
      }
      if (tile.waitingForKey && !key) {
        continue;
      }
      tile.waitingForKey = false;
      try {
        tile.decoder!.decode(
          new EncodedVideoChunk({ type: key ? "key" : "delta", timestamp: tile.timestamp, data: bytes.subarray(2) }),
        );
      } catch {
        tile.waitingForKey = true;
      }
      tile.timestamp += Math.round(1_000_000 / VIDEO_FPS);
    }
  }

  private tile(channel: number): Tile {
    const known = this.tiles.get(channel);
    if (known !== undefined) {
      return known;
    }
    const canvas = document.createElement("canvas");
    canvas.className = "app-video-tile";
    canvas.width = VIDEO_WIDTH;
    canvas.height = VIDEO_HEIGHT;
    canvas.setAttribute("data-channel", String(channel));
    canvas.setAttribute("aria-label", channel === 0 ? "Your own picture, sent back by the app" : `Person ${channel}`);
    this.host.appendChild(canvas);
    const tile: Tile = { canvas, decoder: null, codec: -1, waitingForKey: true, timestamp: 0, last: Date.now() };
    this.tiles.set(channel, tile);
    return tile;
  }

  private configure(tile: Tile, codec: number): void {
    if (tile.decoder !== null && tile.decoder.state !== "closed") {
      tile.decoder.close();
    }
    tile.decoder = null;
    const known = VIDEO_CODECS.find((c) => c.id === codec);
    if (known === undefined) {
      return;
    }
    const context = tile.canvas.getContext("2d");
    const decoder = new VideoDecoder({
      output: (frame) => {
        if (tile.canvas.width !== frame.displayWidth || tile.canvas.height !== frame.displayHeight) {
          tile.canvas.width = frame.displayWidth;
          tile.canvas.height = frame.displayHeight;
        }
        context?.drawImage(frame, 0, 0);
        frame.close();
        this.shown += 1;
        this.onShown(this.shown);
      },
      error: () => {
        // Start again at the next key frame.
        tile.decoder = null;
        tile.waitingForKey = true;
      },
    });
    decoder.configure({ codec: known.codec, optimizeForLatency: true });
    tile.decoder = decoder;
    tile.codec = codec;
    tile.waitingForKey = true;
  }

  private drop(channel: number): void {
    const tile = this.tiles.get(channel);
    if (tile === undefined) {
      return;
    }
    if (tile.decoder !== null && tile.decoder.state !== "closed") {
      tile.decoder.close();
    }
    tile.canvas.remove();
    this.tiles.delete(channel);
  }

  stop(): void {
    clearInterval(this.sweep);
    for (const channel of Array.from(this.tiles.keys())) {
      this.drop(channel);
    }
  }
}
