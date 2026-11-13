// "Continue as new wave": when the host of a wave of another domain is
// gone for good (its person's lease ran out and none of its devices
// answers, or the device holding it was revoked), any participant may
// carry the conversation on in a new wave of their own: the same blips,
// text and participants, and a link back to the old wave.

import { useEffect, useState } from "preact/hooks";

import { type HostStatus, personErrorText } from "../../net/person";

/** How often a wave of another domain is asked about. */
export const HOST_CHECK_MS = 30_000;

export interface ContinueWaveApi {
  hostStatus(waveId: string): Promise<HostStatus>;
  continueWave(waveId: string): Promise<string>;
}

/** Whether a wave of `waveDomain` is worth asking about from `localDomain`. */
export function isRemoteWave(waveDomain: string, localDomain: string | null): boolean {
  return localDomain !== null && waveDomain !== "" && waveDomain !== localDomain;
}

export function ContinueWaveNotice(props: {
  readonly api: ContinueWaveApi;
  readonly waveId: string;
  /** Goes to the new wave. */
  readonly onContinued: (waveId: string) => void;
}) {
  const { api, waveId, onContinued } = props;
  const [status, setStatus] = useState<HostStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    const check = (): void => {
      api.hostStatus(waveId).then(
        (answer) => {
          if (live) {
            setStatus(answer);
          }
        },
        () => {},
      );
    };
    check();
    const timer = setInterval(check, HOST_CHECK_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [api, waveId]);

  if (status === null || !status.frozen) {
    return null;
  }
  const go = (): void => {
    setBusy(true);
    setError(null);
    api.continueWave(waveId).then(
      (newWave) => {
        setBusy(false);
        onContinued(newWave);
      },
      (failure: unknown) => {
        setBusy(false);
        setError(personErrorText(failure));
      },
    );
  };
  return (
    <div class="notice notice-warning wave-frozen" role="status" data-host={status.host}>
      <span>
        This wave's host is gone{status.reason !== "" ? ` (${status.reason})` : ""}. You can carry the
        conversation on in a new wave of your own.
        {error !== null ? <span class="wave-frozen-error"> {error}</span> : null}
      </span>
      <button type="button" class="btn btn-primary" data-action="continue-wave" disabled={busy} onClick={go}>
        {busy ? "Continuing…" : "Continue as new wave"}
      </button>
    </div>
  );
}
