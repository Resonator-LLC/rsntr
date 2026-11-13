// The parts of the wave panel around the conversation: participants bar,
// toolbars, tags bar, the blip menu and the dialogs.
//
// Wording and behaviour follow the Apache Wave web client where it has the
// feature (`ParticipantController`, `MenuController`, `ViewToolbar`,
// `BlipLinkPopupWidget`, `AttachmentPopupWidget` and their message
// bundles, Apache License 2.0).

import { useContext, useEffect, useRef, useState } from "preact/hooks";

import type { DirectoryEntry, ProbeResult } from "../../net/http";
import { Avatar } from "../profiles/Avatar";
import { baseFileName } from "./attachments";
import { type WaveUi, WaveUiContext } from "./context";
import { Dialog } from "./Dialog";
import {
  ArchiveIcon,
  AttachIcon,
  CheckAllIcon,
  ClockIcon,
  CloseIcon,
  CopyIcon,
  DeleteIcon,
  DoneIcon,
  DownIcon,
  EditIcon,
  InboxIcon,
  InlineReplyIcon,
  LinkIcon,
  PlusIcon,
  ReplyIcon,
  UnreadIcon,
  UpIcon,
} from "./icons";
import { MuteIcon } from "../app/icons";
import { parseParticipantInput } from "./participants";
import { Popup } from "./Popup";
import { ROOT_THREAD } from "./traversal";
import { editorToolbar } from "./viewFactory";
import { formatBytes } from "../editor/render";

function useUi(): WaveUi {
  const ui = useContext(WaveUiContext);
  if (ui === null) {
    throw new Error("wave panel component used outside a wave");
  }
  return ui;
}

// ---- participants ----

/** The domain of `address` if it is not this server's; null otherwise or if unknown. */
export function remoteDomain(localDomain: string | null, address: string): string | null {
  const at = address.lastIndexOf("@");
  if (localDomain === null || at === -1) {
    return null;
  }
  const domain = address.slice(at + 1);
  return domain !== "" && domain !== localDomain ? domain : null;
}

/** A domain short enough for a badge: its first label, at most 8 characters. */
export function shortDomain(domain: string): string {
  const label = domain.split(".")[0] ?? domain;
  return label.length > 8 ? `${label.slice(0, 6)}…` : label;
}

export function ParticipantsBar() {
  const ui = useUi();
  const participants = ui.store.participants.value;
  return (
    <div class="wave-participants" role="group" aria-label="Participants">
      <ul class="wave-participant-list">
        {participants.map((address) => {
          const profile = ui.profiles.get(address);
          const remote = remoteDomain(ui.localDomain, address);
          return (
            <li key={address}>
              <button
                type="button"
                class={remote !== null ? "wave-participant wave-participant-remote" : "wave-participant"}
                aria-label={`${profile.name} (${address})`}
                title={remote !== null ? `${profile.name} (${address}, on another server)` : undefined}
                aria-haspopup="dialog"
                data-participant={address}
                onClick={(event) => {
                  const anchor = event.currentTarget as HTMLElement;
                  ui.participantCard.value =
                    ui.participantCard.peek()?.address === address ? null : { address, anchor };
                }}
              >
                <Avatar profiles={ui.profiles} address={address} size="lg" decorative />
                {remote !== null ? (
                  <span class="wave-participant-domain" aria-hidden="true">
                    {shortDomain(remote)}
                  </span>
                ) : null}
              </button>
            </li>
          );
        })}
      </ul>
      <button
        type="button"
        class="icon-btn wave-add-participant"
        aria-label="Add participant"
        title="Add participant"
        onClick={() => {
          ui.dialog.value = { kind: "addParticipant" };
        }}
      >
        <PlusIcon />
      </button>
    </div>
  );
}

/** The profile popup of a participant, with "Remove" (`handleParticipantClicked`). */
export function ParticipantCard() {
  const ui = useUi();
  const card = ui.participantCard.value;
  if (card === null) {
    return null;
  }
  const profile = ui.profiles.get(card.address);
  const close = (): void => {
    ui.participantCard.value = null;
  };
  return (
    <Popup anchor={card.anchor} onClose={close} role="dialog" label={`Participant ${profile.name}`}>
      <div class="wave-card-body">
        <Avatar profiles={ui.profiles} address={card.address} size="lg" decorative />
        <div class="wave-card-text">
          <div class="wave-card-name">{profile.name}</div>
          <div class="wave-card-address">{card.address}</div>
        </div>
      </div>
      <div class="wave-card-actions">
        <button
          type="button"
          class="btn"
          onClick={() => {
            close();
            if (card.address === ui.viewer) {
              ui.dialog.value = { kind: "removeSelf", address: card.address };
            } else {
              ui.store.removeParticipant(card.address);
            }
          }}
        >
          Remove
        </button>
      </div>
    </Popup>
  );
}

/** How long typing must pause before the directory is asked again. */
const SUGGEST_DELAY_MS = 150;

/** What is known of a remote address being added. */
export type ProbeState = ProbeResult | "checking" | "failed";

/** The full address of another domain typed in, if federation is on and it is not in the wave yet. */
export function remoteCandidate(ui: WaveUi, term: string, participants: readonly string[]): string | null {
  if (ui.probe === null || !/^[^@\s,]+@[^@\s,]+$/.test(term)) {
    return null;
  }
  const address = term.toLowerCase();
  if (remoteDomain(ui.localDomain, address) === null || participants.includes(address)) {
    return null;
  }
  return address;
}

export function probeStateName(state: ProbeState): string {
  if (state === "checking" || state === "failed") {
    return state;
  }
  if (!state.allowed) {
    return "not-allowed";
  }
  if (!state.reachable) {
    return "unreachable";
  }
  return state.exists === false ? "unknown-address" : "reachable";
}

export function describeProbe(state: ProbeState, domain: string): string {
  switch (probeStateName(state)) {
    case "checking":
      return `Checking ${domain}…`;
    case "failed":
      return "Cannot check right now";
    case "not-allowed":
      return `Not allowed: this server does not federate with ${domain}`;
    case "unreachable":
      return `${domain} is unreachable right now; it gets the wave once it is back`;
    case "unknown-address":
      return `${domain} does not know this address`;
    default:
      return `Reachable on ${domain}`;
  }
}

function AddParticipantDialog({ onClose }: { onClose: () => void }) {
  const ui = useUi();
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [found, setFound] = useState<readonly DirectoryEntry[]>([]);
  const participants = ui.store.participants.value;
  // What is being typed now: the text after the last comma.
  const term = (value.split(",").pop() ?? "").trim();
  useEffect(() => {
    const directory = ui.directory;
    if (directory === null) {
      return;
    }
    let current = true;
    const timer = setTimeout(
      () => {
        directory(term).then(
          (entries) => {
            if (current) {
              setFound(entries);
            }
          },
          () => {
            if (current) {
              setFound([]);
            }
          },
        );
      },
      term === "" ? 0 : SUGGEST_DELAY_MS,
    );
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [ui.directory, term]);
  const suggestions = found.filter((entry) => !participants.includes(entry.address));
  // With federation, a full address of another domain: probed and offered.
  const remoteAddress = remoteCandidate(ui, term, participants);
  const [probed, setProbed] = useState<{ readonly address: string; readonly result: ProbeState } | null>(null);
  useEffect(() => {
    const probe = ui.probe;
    if (probe === null || remoteAddress === null) {
      return;
    }
    let current = true;
    const timer = setTimeout(() => {
      setProbed({ address: remoteAddress, result: "checking" });
      probe(remoteAddress).then(
        (result) => {
          if (current) {
            setProbed({ address: remoteAddress, result });
          }
        },
        () => {
          if (current) {
            setProbed({ address: remoteAddress, result: "failed" });
          }
        },
      );
    }, SUGGEST_DELAY_MS);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [ui.probe, remoteAddress]);
  const remoteState: ProbeState = probed !== null && probed.address === remoteAddress ? probed.result : "checking";
  const submit = (event: Event): void => {
    event.preventDefault();
    if (value.trim() === "") {
      onClose();
      return;
    }
    const parsed = parseParticipantInput(ui.localDomain, value);
    if (!parsed.ok) {
      setError(parsed.message);
      return;
    }
    ui.store.addParticipants(parsed.participants.map((p) => p.address));
    onClose();
  };
  // Adds a suggested account at once; the dialog stays open for more.
  const pick = (entry: DirectoryEntry): void => {
    ui.store.addParticipants([entry.address]);
    const typed = value.split(",");
    typed.pop();
    setValue(typed.length > 0 ? `${typed.join(",")}, ` : "");
    setError(null);
  };
  return (
    <Dialog title="Add participant" onClose={onClose}>
      <form onSubmit={submit}>
        <div class="dialog-body">
          <div class="field">
            <label for="wave-add-participant-input">Name or address (separate several with commas):</label>
            <input
              id="wave-add-participant-input"
              class="input"
              type="text"
              autocomplete="off"
              autocapitalize="none"
              spellcheck={false}
              value={value}
              aria-invalid={error !== null}
              aria-describedby={error !== null ? "wave-add-participant-error" : undefined}
              aria-controls={ui.directory !== null ? "wave-add-participant-suggestions" : undefined}
              onInput={(event) => {
                setValue((event.currentTarget as HTMLInputElement).value);
                setError(null);
              }}
            />
            {error !== null ? (
              <div id="wave-add-participant-error" class="field-error" role="alert">
                {error}
              </div>
            ) : null}
          </div>
          {remoteAddress !== null ? (
            <div class="suggestions suggestions-remote">
              <div class="suggestions-title" id="wave-add-participant-remote-title">
                On another server
              </div>
              <ul
                id="wave-add-participant-remote"
                class="suggestion-list"
                aria-labelledby="wave-add-participant-remote-title"
              >
                <li>
                  <button
                    type="button"
                    class="suggestion suggestion-remote"
                    data-address={remoteAddress}
                    data-probe={probeStateName(remoteState)}
                    onClick={() => pick({ address: remoteAddress, name: remoteAddress, kind: "human" })}
                  >
                    <Avatar profiles={ui.profiles} address={remoteAddress} size="sm" decorative />
                    <span class="suggestion-text">
                      <span class="suggestion-name">{remoteAddress}</span>
                      <span
                        class="suggestion-remote-status"
                        data-state={probeStateName(remoteState) === "reachable" ? "ok" : "warn"}
                        role="status"
                      >
                        {describeProbe(remoteState, remoteDomain(ui.localDomain, remoteAddress) ?? "")}
                      </span>
                    </span>
                    <span class="suggestion-add" aria-hidden="true">
                      Add
                    </span>
                  </button>
                </li>
              </ul>
            </div>
          ) : null}
          {ui.directory !== null ? (
            <div class="suggestions">
              <div class="suggestions-title" id="wave-add-participant-suggestions-title">
                {term === "" ? "People on this server" : "Matches"}
              </div>
              {suggestions.length === 0 ? (
                <div class="suggestions-empty">
                  {found.length > 0 ? "Everyone listed is already in this wave." : "Nobody found."}
                </div>
              ) : (
                <ul
                  id="wave-add-participant-suggestions"
                  class="suggestion-list"
                  aria-labelledby="wave-add-participant-suggestions-title"
                >
                  {suggestions.map((entry) => (
                    <li key={entry.address}>
                      <button
                        type="button"
                        class="suggestion"
                        data-address={entry.address}
                        onClick={() => pick(entry)}
                      >
                        <Avatar profiles={ui.profiles} address={entry.address} size="sm" decorative />
                        <span class="suggestion-text">
                          <span class="suggestion-name">
                            {entry.name}
                            {entry.kind !== "human" ? (
                              <span class="suggestion-kind">{entry.kind === "agent" ? "AI" : "robot"}</span>
                            ) : null}
                          </span>
                          <span class="suggestion-address">{entry.address}</span>
                        </span>
                        <span class="suggestion-add" aria-hidden="true">
                          Add
                        </span>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          ) : null}
        </div>
        <div class="dialog-actions">
          <button type="button" class="btn" onClick={onClose}>
            Close
          </button>
          <button type="submit" class="btn btn-primary">
            {value.trim() === "" ? "Done" : "Add"}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

// ---- tags ----

export function TagsBar() {
  const ui = useUi();
  const tags = ui.store.tags.value;
  const [adding, setAdding] = useState(false);
  const [value, setValue] = useState("");
  const addRef = useRef<HTMLButtonElement>(null);
  const finish = (add: boolean): void => {
    if (add) {
      for (const tag of value.split(",")) {
        ui.store.addTag(tag);
      }
    }
    setValue("");
    setAdding(false);
  };
  return (
    <div class="wave-tags" role="group" aria-label="Tags">
      <span class="wave-tags-label" aria-hidden="true">
        Tags:
      </span>
      <ul class="wave-tag-list">
        {tags.map((tag) => (
          <li key={tag} class="wave-tag">
            <span class="wave-tag-name">{tag}</span>
            <button
              type="button"
              class="wave-tag-remove"
              aria-label={`Remove tag ${tag}`}
              title="Remove tag"
              onClick={() => ui.store.removeTag(tag)}
            >
              <CloseIcon />
            </button>
          </li>
        ))}
      </ul>
      {adding ? (
        <form
          class="wave-tag-form"
          onSubmit={(event) => {
            event.preventDefault();
            finish(true);
            addRef.current?.focus();
          }}
        >
          <input
            class="input wave-tag-input"
            type="text"
            aria-label="New tag"
            placeholder="New tag"
            autocomplete="off"
            value={value}
            ref={(el) => el?.focus()}
            onInput={(event) => setValue((event.currentTarget as HTMLInputElement).value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                finish(false);
              }
            }}
            onBlur={() => finish(value.trim() !== "")}
          />
        </form>
      ) : (
        <button
          ref={addRef}
          type="button"
          class="icon-btn wave-tag-add"
          aria-label="Add tag"
          title="Add tag"
          onClick={() => setAdding(true)}
        >
          <PlusIcon />
        </button>
      )}
    </div>
  );
}

// ---- toolbars ----

/** The read-mode toolbar (`ViewToolbar`: Recent, Next unread, Previous, Next), plus archive and read state. */
function ViewToolbar() {
  const ui = useUi();
  const store = ui.store;
  const archived = store.archived.value;
  const followed = store.followed.value;
  const unread = store.threads.get(ROOT_THREAD)?.unread.value ?? 0;
  return (
    <div class="wave-toolbar" role="toolbar" aria-label="Wave">
      <button type="button" class="btn btn-ghost wave-tool" onClick={() => store.focusMostRecent()}>
        <ClockIcon />
        <span class="wave-tool-label">Recent</span>
      </button>
      <button type="button" class="btn btn-ghost wave-tool" data-action="next-unread" onClick={() => store.nextUnread()}>
        <UnreadIcon />
        <span class="wave-tool-label">Next unread</span>
        {unread > 0 ? (
          <span class="badge-unread" aria-label={`${unread} unread`}>
            {unread}
          </span>
        ) : null}
      </button>
      <button type="button" class="btn btn-ghost wave-tool" aria-label="Previous" title="Previous" onClick={() => store.moveUp()}>
        <UpIcon />
        <span class="wave-tool-label wave-tool-label-wide">Previous</span>
      </button>
      <button type="button" class="btn btn-ghost wave-tool" aria-label="Next" title="Next" onClick={() => store.moveDown()}>
        <DownIcon />
        <span class="wave-tool-label wave-tool-label-wide">Next</span>
      </button>
      <span class="wave-toolbar-gap" />
      {unread > 0 ? (
        <button type="button" class="btn btn-ghost wave-tool" title="Mark the whole wave as read" onClick={() => store.markAllRead()}>
          <CheckAllIcon />
          <span class="wave-tool-label wave-tool-label-wide">Mark read</span>
        </button>
      ) : null}
      <button
        type="button"
        class="btn btn-ghost wave-tool"
        data-action={archived ? "to-inbox" : "to-archive"}
        onClick={() => store.setArchived(!archived)}
      >
        {archived ? <InboxIcon /> : <ArchiveIcon />}
        <span class="wave-tool-label wave-tool-label-wide">{archived ? "To Inbox" : "To Archive"}</span>
      </button>
      <button
        type="button"
        class="btn btn-ghost wave-tool"
        data-action={followed ? "mute" : "unmute"}
        aria-pressed={!followed}
        title={followed ? "Mute: stop following this wave" : "Unmute: follow this wave again"}
        onClick={() => store.setFollowed(!followed)}
      >
        <MuteIcon />
        <span class="wave-tool-label wave-tool-label-wide">{followed ? "Mute" : "Unmute"}</span>
      </button>
    </div>
  );
}

/** While a blip is edited: the editor's toolbar, or a minimal one if the editor has none. */
function EditingToolbar() {
  const ui = useUi();
  const store = ui.store;
  const view = store.editingView.value;
  if (view === null) {
    return <div class="wave-toolbar wave-edit-toolbar" role="toolbar" aria-label="Editing" />;
  }
  // Straight to the system's file chooser; the upload starts once a file
  // is chosen (`AttachmentDialog` shows its progress, or why it failed).
  const onInsertAttachment = (): void => {
    for (const stale of document.querySelectorAll("input.wave-file-input")) {
      stale.remove();
    }
    const input = document.createElement("input");
    input.type = "file";
    input.className = "wave-file-input";
    input.hidden = true;
    input.setAttribute("aria-label", "File to upload");
    input.addEventListener("change", () => {
      const file = input.files?.[0];
      input.remove();
      if (file !== undefined) {
        ui.dialog.value = { kind: "attachment", file };
      }
    });
    input.addEventListener("cancel", () => input.remove());
    document.body.append(input);
    input.click();
  };
  const onInsertInlineReply = (): void => store.inlineReplyAtCaret();
  const onDone = (): void => store.stopEditing();
  const EditorToolbar = editorToolbar();
  if (EditorToolbar !== null) {
    return (
      <div class="wave-edit-toolbar">
        <EditorToolbar
          view={view}
          onInsertAttachment={onInsertAttachment}
          onInsertInlineReply={onInsertInlineReply}
          onDone={onDone}
        />
      </div>
    );
  }
  // Keeps the caret in the document when a button is pressed.
  const keepSelection = (event: Event): void => event.preventDefault();
  return (
    <div class="wave-toolbar wave-edit-toolbar" role="toolbar" aria-label="Editing" onMouseDown={keepSelection}>
      <button type="button" class="btn btn-ghost wave-tool" data-action="attachment" onClick={onInsertAttachment}>
        <AttachIcon />
        <span class="wave-tool-label">Attachment</span>
      </button>
      <button type="button" class="btn btn-ghost wave-tool" data-action="inline-reply" onClick={onInsertInlineReply}>
        <InlineReplyIcon />
        <span class="wave-tool-label">Inline reply</span>
      </button>
      <span class="wave-toolbar-gap" />
      <button type="button" class="btn btn-primary wave-tool" data-action="done" onClick={onDone}>
        <DoneIcon />
        <span class="wave-tool-label">Done</span>
      </button>
    </div>
  );
}

/** The toolbar row: the edit toolbar during an edit session, else the view toolbar (`ToolbarSwitcher`). */
export function Toolbar() {
  const ui = useUi();
  return ui.store.editingId.value !== null ? <EditingToolbar /> : <ViewToolbar />;
}

// ---- blip menu ----

/** The menu of a blip: Reply, Edit or Done, Delete, Link (`MenuController`). */
export function BlipMenu() {
  const ui = useUi();
  const store = ui.store;
  const menu = ui.menu.value;
  if (menu === null) {
    return null;
  }
  const { blipId, anchor } = menu;
  const editing = store.editingId.value === blipId;
  const close = (): void => {
    ui.menu.value = null;
  };
  const act = (action: () => void) => (): void => {
    close();
    action();
  };
  return (
    <Popup anchor={anchor} onClose={close} role="menu" label="Message actions">
      {editing ? (
        <button type="button" class="menu-item" role="menuitem" data-action="done" onClick={act(() => store.stopEditing())}>
          <DoneIcon />
          Done
        </button>
      ) : (
        <>
          <button type="button" class="menu-item" role="menuitem" data-action="reply" onClick={act(() => store.reply(blipId))}>
            <ReplyIcon />
            Reply
          </button>
          <button type="button" class="menu-item" role="menuitem" data-action="edit" onClick={act(() => store.startEditing(blipId))}>
            <EditIcon />
            Edit
          </button>
          <button
            type="button"
            class="menu-item menu-item-danger"
            role="menuitem"
            data-action="delete"
            onClick={(event) => {
              close();
              // As the original: no confirmation with the shift key held.
              if (event.shiftKey) {
                store.deleteBlip(blipId);
              } else {
                ui.dialog.value = { kind: "delete", blipId };
              }
            }}
          >
            <DeleteIcon />
            Delete
          </button>
          <button
            type="button"
            class="menu-item"
            role="menuitem"
            data-action="link"
            onClick={act(() => {
              ui.dialog.value = { kind: "link", blipId };
            })}
          >
            <LinkIcon />
            Link
          </button>
        </>
      )}
    </Popup>
  );
}

// ---- dialogs ----

function ConfirmDialog(props: {
  title: string;
  message: string;
  confirm: string;
  danger?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <Dialog title={props.title} onClose={props.onClose}>
      <div class="dialog-body">
        <p>{props.message}</p>
      </div>
      <div class="dialog-actions">
        <button type="button" class="btn" onClick={props.onClose}>
          Cancel
        </button>
        <button
          type="button"
          class={`btn ${props.danger === true ? "btn-danger" : "btn-primary"}`}
          data-action="confirm"
          autofocus
          onClick={() => {
            props.onClose();
            props.onConfirm();
          }}
        >
          {props.confirm}
        </button>
      </div>
    </Dialog>
  );
}

function LinkDialog({ blipId, onClose }: { blipId: string; onClose: () => void }) {
  const ui = useUi();
  const url = ui.linkForBlip(blipId);
  const inputRef = useRef<HTMLInputElement>(null);
  const [copied, setCopied] = useState<"no" | "yes" | "failed">("no");
  const copy = (): void => {
    const done = (ok: boolean): void => setCopied(ok ? "yes" : "failed");
    const fallback = (): void => {
      inputRef.current?.select();
      let ok = false;
      try {
        ok = document.execCommand("copy");
      } catch {
        ok = false;
      }
      done(ok);
    };
    if (navigator.clipboard?.writeText !== undefined) {
      navigator.clipboard.writeText(url).then(() => done(true), fallback);
    } else {
      fallback();
    }
  };
  return (
    <Dialog title="Link to this blip" onClose={onClose}>
      <div class="dialog-body">
        <div class="field">
          <label for="wave-link-input">Use this reference to link to this blip from the web</label>
          <div class="wave-link-row">
            <input
              id="wave-link-input"
              ref={inputRef}
              class="input"
              type="text"
              readOnly
              value={url}
              onFocus={(event) => (event.currentTarget as HTMLInputElement).select()}
            />
            <button type="button" class="btn" data-action="copy" onClick={copy}>
              <CopyIcon />
              Copy
            </button>
          </div>
          <div class="wave-link-status" role="status">
            {copied === "yes" ? "Copied" : copied === "failed" ? "Could not copy; select the text and copy it" : ""}
          </div>
        </div>
      </div>
      <div class="dialog-actions">
        <button type="button" class="btn btn-primary" onClick={onClose}>
          Close
        </button>
      </div>
    </Dialog>
  );
}

/**
 * The upload of a chosen file (after `AttachmentPopupWidget`, without its
 * form: the file comes from the system's chooser): uploads it, inserts it
 * at the caret and closes; stays open only to say why that failed.
 */
function AttachmentDialog({ file, onClose }: { file: File; onClose: () => void }) {
  const ui = useUi();
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    const attachments = ui.attachments;
    const view = ui.store.editingView.peek();
    if (attachments === null || view === null) {
      setError("Error! The message is no longer being edited.");
      return;
    }
    const attachmentId = ui.newAttachmentId();
    attachments.upload(attachmentId, file, file.name).then(
      () => {
        // The session may have ended or moved on while the file went up.
        if (ui.store.editingView.peek() !== view) {
          setError("The file was uploaded, but the message is no longer being edited.");
          return;
        }
        view.exec({ type: "insertAttachment", attachmentId, caption: baseFileName(file.name) });
        onClose();
      },
      (failure: unknown) => {
        if (failure instanceof Error && (failure as { status?: number }).status === 413) {
          setError(`"${file.name}" (${formatBytes(file.size)}) is too large to attach here.`);
          return;
        }
        const detail = failure instanceof Error && failure.message !== "" ? `: ${failure.message}` : "";
        setError(`Error! The upload failed${detail}`);
      },
    );
  }, []);
  return (
    <Dialog title="Upload attachment" onClose={onClose}>
      <div class="dialog-body">
        {error === null ? (
          <div class="wave-upload-status" role="status">
            <span class="wave-spinner" aria-hidden="true" />
            Uploading {baseFileName(file.name)}...
          </div>
        ) : (
          <div class="notice notice-error" role="alert">
            {error}
          </div>
        )}
      </div>
      <div class="dialog-actions">
        <button type="button" class="btn" onClick={onClose}>
          {error === null ? "Cancel" : "Close"}
        </button>
      </div>
    </Dialog>
  );
}

export function Dialogs() {
  const ui = useUi();
  const dialog = ui.dialog.value;
  if (dialog === null) {
    return null;
  }
  const close = (): void => {
    if (ui.dialog.peek() === dialog) {
      ui.dialog.value = null;
    }
  };
  switch (dialog.kind) {
    case "delete":
      return (
        <ConfirmDialog
          title="Delete message"
          message="Please confirm the deletion of this message"
          confirm="Delete"
          danger
          onConfirm={() => ui.store.deleteBlip(dialog.blipId)}
          onClose={close}
        />
      );
    case "removeSelf":
      return (
        <ConfirmDialog
          title="Leave this wave"
          message="Remove yourself from this wave? You will no longer be able to open it."
          confirm="Remove"
          danger
          onConfirm={() => ui.store.removeParticipant(dialog.address)}
          onClose={close}
        />
      );
    case "link":
      return <LinkDialog blipId={dialog.blipId} onClose={close} />;
    case "addParticipant":
      return <AddParticipantDialog onClose={close} />;
    case "attachment":
      return <AttachmentDialog file={dialog.file} onClose={close} />;
  }
}
