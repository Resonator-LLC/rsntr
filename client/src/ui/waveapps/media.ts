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

/** Whether this browser can capture and encode, and decode and play. */
export function mediaSupported(): boolean {
  return (
    typeof AudioEncoder !== "undefined" &&
    typeof AudioDecoder !== "undefined" &&
    typeof AudioContext !== "undefined" &&
    typeof navigator !== "undefined" &&
    navigator.mediaDevices?.getUserMedia !== undefined
  );
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

  static async start(send: (frame: string) => void): Promise<Mic> {
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true, channelCount: 1 },
    });
    const context = new AudioContext({ sampleRate: SAMPLE_RATE });
    const encoder = new AudioEncoder({
      output: (chunk) => {
        const bytes = new Uint8Array(chunk.byteLength);
        chunk.copyTo(bytes);
        send(base64(bytes));
      },
      error: () => {},
    });
    encoder.configure({ codec: "opus", sampleRate: SAMPLE_RATE, numberOfChannels: 1, bitrate: 24_000 });
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

  /** `onPlayed` is told the number of frames played so far. */
  constructor(private readonly onPlayed: (played: number) => void = () => {}) {}

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
    this.context ??= new AudioContext({ sampleRate: SAMPLE_RATE });
    const context = this.context;
    const state = { decoder: null as unknown as AudioDecoder, next: 0, timestamp: 0 };
    state.decoder = new AudioDecoder({
      output: (data) => {
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
