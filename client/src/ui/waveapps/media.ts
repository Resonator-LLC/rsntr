// Sound for wave apps with the `media` permission. The microphone is
// captured here, encoded to Opus with WebCodecs and sent to the app one
// frame at a time; what the app sends back (a channel byte, then an Opus
// frame) is decoded per channel and played. The app on the node never
// touches the devices; the person turns the microphone on and off in the
// header we draw.

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
