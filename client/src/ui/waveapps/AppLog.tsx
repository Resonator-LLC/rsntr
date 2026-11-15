// The Log of a wave app: its app wavelet's messages, read-only, kept up
// to date while open.

import { useEffect, useState } from "preact/hooks";

import type { OpenWave } from "../../client";
import type { OpBasedWavelet } from "../../model/wavelet";
import { Dialog } from "../wave/Dialog";
import { formatFullDate } from "../wave/dates";
import { type AppLogMessage, appLogMessages, appWaveletId } from "./log";

/** Most messages shown, the newest. */
const SHOWN = 500;

export function AppLog({
  wave,
  gadgetId,
  name,
  onClose,
}: {
  readonly wave: OpenWave;
  readonly gadgetId: string;
  readonly name: string;
  readonly onClose: () => void;
}) {
  const [messages, setMessages] = useState<readonly AppLogMessage[]>([]);
  useEffect(() => {
    const id = appWaveletId(wave.waveId.domain, gadgetId);
    let wavelet: OpBasedWavelet | null = null;
    let unsubscribe: (() => void) | null = null;
    const read = (): void => setMessages(wavelet === null ? [] : appLogMessages(wavelet.data()));
    const attach = (): void => {
      unsubscribe?.();
      wavelet = wave.view.wavelet(id);
      unsubscribe = wavelet?.events.on("documentChanged", read) ?? null;
      read();
    };
    attach();
    const offAdded = wave.view.events.on("waveletAdded", ({ waveletId }) => {
      if (waveletId.equals(id)) {
        attach();
      }
    });
    return () => {
      offAdded();
      unsubscribe?.();
    };
  }, [wave, gadgetId]);

  const shown = messages.slice(-SHOWN);
  return (
    <Dialog title={`${name} log`} onClose={onClose}>
      <div class="dialog-body app-log">
        {shown.length === 0 ? (
          <p class="app-log-empty">No messages yet.</p>
        ) : (
          <ol class="app-log-list">
            {shown.map((m) => (
              <li key={m.seq} class="app-log-item">
                <div class="app-log-meta">
                  <span class="app-log-author">{m.author}</span>
                  <span class="app-log-kind">{m.kind}</span>
                  {m.time !== null ? <span class="app-log-time">{formatFullDate(m.time)}</span> : null}
                </div>
                <pre class="app-log-body">{m.body}</pre>
              </li>
            ))}
          </ol>
        )}
      </div>
      <div class="dialog-actions">
        <button type="button" class="btn btn-primary" onClick={onClose}>
          Close
        </button>
      </div>
    </Dialog>
  );
}
