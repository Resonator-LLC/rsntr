// The wave panel: shows the wave the route names, or says why it cannot.

import { useEffect, useState } from "preact/hooks";

import "../styles/wave.css";

import type { OpenWave, WaveClient } from "../../client";
import type { WavePanelProps } from "../contracts";
import { WaveId } from "../../model/id";
import { ContinueWaveNotice, isRemoteWave } from "./ContinueWave";
import { BackIcon } from "./icons";
import { WaveView } from "./WaveView";

// The panel closes the waves it showed, but a wave may be shown again
// before its last changes were saved and it was closed.
const holds = new WeakMap<OpenWave, { count: number }>();

function acquire(client: WaveClient, waveId: string): OpenWave {
  const wave = client.attachWave(waveId);
  const hold = holds.get(wave) ?? { count: 0 };
  hold.count++;
  holds.set(wave, hold);
  return wave;
}

function release(wave: OpenWave): void {
  const hold = holds.get(wave);
  if (hold === undefined || --hold.count > 0) {
    return;
  }
  const close = (): void => {
    if (hold.count === 0) {
      wave.close();
    }
  };
  if (wave.state === "open" && !wave.unsavedState.saved) {
    // Nothing is sent after closing: let the last changes go out first.
    wave.whenSaved().then(close, close);
  } else {
    close();
  }
}

type Status =
  | { readonly kind: "none" }
  | { readonly kind: "loading"; readonly wave: OpenWave }
  | { readonly kind: "open"; readonly wave: OpenWave }
  | { readonly kind: "closed" }
  | { readonly kind: "failed"; readonly message: string; readonly notAuthorized: boolean };

function statusOf(wave: OpenWave): Status {
  switch (wave.state) {
    case "open":
      return { kind: "open", wave };
    case "failed": {
      const error = wave.live.error;
      return {
        kind: "failed",
        message: error?.message ?? "The wave stopped working.",
        notAuthorized: error?.notAuthorized ?? false,
      };
    }
    case "closed":
      return { kind: "closed" };
    default:
      return { kind: "loading", wave };
  }
}

export function WavePanel(props: WavePanelProps) {
  const { client, route } = props;
  const waveId = route.waveId;
  const [status, setStatus] = useState<Status>({ kind: "none" });
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    if (waveId === null) {
      setStatus({ kind: "none" });
      return;
    }
    let wave: OpenWave;
    try {
      wave = acquire(client, waveId);
    } catch (error) {
      setStatus({
        kind: "failed",
        message: error instanceof Error ? error.message : String(error),
        notAuthorized: false,
      });
      return;
    }
    const update = (): void => setStatus(statusOf(wave));
    update();
    const offs = [
      wave.events.on("opened", update),
      wave.events.on("failed", update),
      wave.events.on("closed", update),
    ];
    return () => {
      offs.forEach((off) => off());
      release(wave);
    };
  }, [client, waveId, attempt]);

  if (status.kind === "open" && waveId !== null) {
    const session = client.session;
    const address = client.address;
    const localDomain = session?.domain ?? address?.domain() ?? null;
    let waveDomain = "";
    try {
      waveDomain = WaveId.deserialise(waveId).domain;
    } catch {
      // Not a wave id: nothing to ask about.
    }
    // Person mode federates: a wave of another person may lose its host.
    const notice =
      session?.device !== undefined && session.federation === true && isRemoteWave(waveDomain, localDomain) ? (
        <ContinueWaveNotice
          key={waveId}
          api={client.api.person}
          waveId={waveId}
          onContinued={(newWave) => {
            props.onWaveChanged();
            props.onNavigate({ waveId: newWave, waveletId: null, blipId: null });
          }}
        />
      ) : null;
    return (
      <WaveView
        key={`${waveId}#${attempt}`}
        wave={status.wave}
        profiles={props.profiles}
        waveletId={route.waveletId}
        blipId={route.blipId}
        startEditing={props.startEditing}
        viewer={address?.address ?? null}
        localDomain={session?.domain ?? address?.domain() ?? null}
        directory={
          // Device mode: the people one knows instead of the server's accounts.
          session?.device !== undefined
            ? (query) => client.api.contacts(query)
            : (query) => client.api.directory(query)
        }
        {...(session?.federation === true ? { probe: (address: string) => client.api.probe(address) } : {})}
        attachmentService={client.api.attachments}
        apps={{
          open: (request, handlers) => client.openApp(request, handlers),
          event: (request) => client.appEvent(request),
        }}
        idSeed={session?.idSeed ?? null}
        onClose={props.onClose}
        onWaveChanged={props.onWaveChanged}
        onNavigate={props.onNavigate}
        linkFor={props.linkFor}
        notice={notice}
      />
    );
  }

  if (status.kind === "none" || waveId === null) {
    return (
      <section class="wave-panel wave-panel-empty" aria-label="Wave">
        <div class="wave-state">
          <p class="wave-state-title">No wave selected</p>
          <p>Select a wave from the list to read it, or start a new wave.</p>
        </div>
      </section>
    );
  }

  let body;
  if (status.kind === "loading") {
    body = (
      <div class="wave-state" role="status">
        <span class="wave-spinner" aria-hidden="true" />
        <p>Loading…</p>
      </div>
    );
  } else if (status.kind === "failed") {
    body = (
      <div class="wave-state" role="alert">
        <p class="wave-state-title">{status.notAuthorized ? "No access" : "The wave could not be shown"}</p>
        <p>
          {status.notAuthorized
            ? "You no longer have access to this wave."
            : status.message}
        </p>
        {status.notAuthorized ? null : (
          <button type="button" class="btn" onClick={() => setAttempt((n) => n + 1)}>
            Try again
          </button>
        )}
      </div>
    );
  } else {
    body = (
      <div class="wave-state" role="status">
        <p class="wave-state-title">Wave closed</p>
        <p>This wave is no longer open.</p>
        <button type="button" class="btn" onClick={() => setAttempt((n) => n + 1)}>
          Open again
        </button>
      </div>
    );
  }
  return (
    <section class="wave-panel" aria-label="Wave">
      <header class="wave-header">
        <button type="button" class="icon-btn wave-back" aria-label="Back to the list of waves" onClick={props.onClose}>
          <BackIcon />
        </button>
        <h2 class="wave-title wave-title-empty">Wave</h2>
      </header>
      {body}
    </section>
  );
}
