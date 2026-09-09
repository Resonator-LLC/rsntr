# One node per device: sharing a node directory between apps

Every app and tool on a device can use the same resonator node: one
identity (`rsntr.key`), one database (`rsntr.db`), one set of peers,
one place where the user's information lives. Apps keep what they know
in that node instead of in private silos, and other apps and remote
peers reach it only through resonator's verbs and policy. This document
is the model; `rsntr guide device` is the short version.

## 1. The directory

| file | who | notes |
|------|-----|-------|
| `rsntr.db` (+`-wal`, `-shm`) | everyone | SQLite in WAL mode: many readers, one writer at a time, coordinated by the OS |
| `rsntr.key` | everyone reads | the device's ed25519 identity, 0600; created once under `rsntr.init.lock` |
| `rsntr.serve.lock`, `rsntr.serve.json` | the serving process | the serving lease (an OS file lock, released by the kernel on any exit) and who holds it |
| `rsntr.sock` | the serving process | the owner channel over a unix socket, where the path fits |
| `rsntr.owner` | the serving process | the owner channel over loopback TCP: port and a random token, 0600 |
| `blobs/` | the serving process | the blob store (an exclusive lock inside) |
| `blobs-inbox/` | any writer | attachments staged for the serving process to import |
| `rsntr.pid`, `rsntr.log` | the `rsntr serve` daemon | desktop only |
| `rsntr.web-token` | `serve --web` | the console's capability token |
| `rsntr.await-cursor` (+`.lock`) | `rsntr await` | one awaiter per cursor file |

Apps may keep sibling files in the directory; prefix them with the
app's slug (`notebook_cursor.txt`), never touch the ones above, and keep
the directory on a local filesystem (no NFS, no FUSE, no cloud-synced
folder: SQLite and file locks need a real filesystem, and a synced copy
of `rsntr.key` would clone the identity onto another device).

## 2. Readers, writers, one server

Any process that can open the directory may read and write the
database directly; the library keeps that safe: every connection waits
for the write lock instead of failing, write transactions take the lock
up front (two writers queue instead of one failing on a snapshot
conflict), migrations run once and atomically, and a current database is
opened without a single write.

At most one process **serves** the node at a time: it runs the network
endpoint (the identity online), delivers the outbox, holds the blob
store, and owns the connection whose changes push vibrations. It holds
the serving lease for as long as it serves. A second `serve()` gets
`AlreadyServing` naming the holder; `rsntr status` shows `app` and
`lease`; `rsntr stop` refuses to touch a lease an app holds.

Every process with a pipeline (a serving process, an app's in-process
pipeline, a long-lived embedder) **watches for commits made by other
processes**: four times a second it checks SQLite's data version, which
moves only when another connection committed, and signals every
entrainment, the hook runner, the outbox, and the projection-changed
point when `_projection`/`_policy` moved. Consumers re-read from their
cursors, so a spurious wake costs one cheap query. Writes made through
the serving process's own connection signal at commit time (never
before, never for a rolled-back statement).

The **owner channel** is the local control channel to the serving
process: a unix socket (`rsntr.sock`) where the path fits, and loopback
TCP with a token (`rsntr.owner`) everywhere. Clients try the socket,
then TCP. It carries three things the server alone does well: push
instead of poll (`rsntr await`, `chat wait`, an app's `entrain` keep a
connection open and the server pushes vibrations), attachment import
into the store it holds locked, and lower latency (a write over the
channel lands on the serving connection). Nothing depends on it for
correctness any more: a write made directly reaches every consumer
through the watcher.

## 3. Who is an owner

Every app and tool that can open the shared directory is the owner of
this node: full power, no policy, every action audited with
`decided_by = owner`. There is no per-app permission on the device, by
design: anyone who can read `rsntr.key` already *is* the node, so an
on-device app policy would be a fiction. What remote peers see is
another matter entirely: they get nothing until a `_policy` row says
otherwise. On phones the shared directory is available to the apps the
user granted it to (iOS) or to the apps of one signing key (Android);
on desktops to every process of the user.

An app with data that must stay private to itself has two honest
options: keep that data in its own private node directory (a second
`Node`, its own key, no sharing) or encrypt the sensitive columns at the
app level before writing them into the shared node. Hiding rows from
other owners by convention is not privacy.

## 4. Platforms

### 4.1 Desktop

`rsntr` uses, when `-d` is omitted: `$RSNTR_NODE`; else the current
directory when it holds `rsntr.db`; else the per-user default
(`~/Library/Application Support/rsntr/node` on macOS,
`$XDG_DATA_HOME/rsntr/node` or `~/.local/share/rsntr/node` on Linux,
`%APPDATA%\rsntr\node` on Windows). `rsntr node dir` prints the
resolution. A bare `rsntr serve` initializes and serves the per-user
node; every local tool of the user then shares it. Windows has no unix
socket; the loopback TCP owner channel covers it.

### 4.2 iOS: a folder in the Files app

One app **hosts** the node in its own `Documents/rsntr`. Its Info.plist
sets `UIFileSharingEnabled` and `LSSupportsOpeningDocumentsInPlace` to
`YES`, so the Files app shows the folder under the app's name. Every
other app, from any vendor, **joins** by letting the user pick that
folder once (`UIDocumentPickerViewController`, folder, in place); the
pick is kept as a bookmark, and later launches open the node directly.
`SharedNode.swift` (in the Swift package) implements both roles and
wires the lifecycle:

- `SharedNode.host(app:)` creates the folder with protection class
  `completeUntilFirstUserAuthentication` (files stay readable while the
  device is locked after its first unlock, which a node another app
  serves in the background needs; do not give the host the Data
  Protection capability at `Complete`) and excludes it from backup (the
  key is the device's identity).
- `SharedNode.openJoined(app:)` resolves the bookmark;
  `SharedNode.open(pickedFolder:app:)` handles the first pick and
  refuses a folder that holds no node (`notANode`).
- On `didEnterBackground` the helper calls `suspend()` inside a
  background task: serving stops, entrainments end with
  `onEnd("closed")`, the database closes, and folder access is released.
  **This is not optional**: iOS terminates a suspended app that holds a
  lock on a file outside its own container (crash reason
  `0xdead10cc`), and SQLite, the blob store, and the lease all hold
  locks. On `willEnterForeground` it calls `resume()`, which takes the
  serving lease when the policy is `serveIfFree` and nobody holds it,
  and runs `onResume`: re-entrain there.
- Whoever is in the foreground serves; the lease moves between apps as
  the user switches. An app with a background mode that keeps running
  may keep serving; the others reach it over loopback TCP.

Not usable: iCloud Drive and third-party file providers. They sync or
proxy files; a live SQLite database in WAL mode needs a local
filesystem, and the identity must not be copied between devices. The
folder must not be renamed, moved, or deleted in Files while apps use
it; deleting the host app deletes the node for everyone (joiners then
see `notANode` and must pick again). App extensions cannot present the
picker; they can only resolve a bookmark the main app stored for them.

### 4.3 Android: one signing key, one uid

Android isolates apps by Linux uid, and a SQLite file cannot be shared
across uids (the Storage Access Framework hands out content URIs, not
paths; external storage is FUSE-mediated and unfit for WAL). Apps that
share a node therefore run as one uid: the same signing key and the
same `android:sharedUserId` in every manifest, declared from each app's
first release (an installed app cannot adopt one later). One package of
the family hosts the directory under its `noBackupFilesDir` (Auto
Backup must not clone the identity); `SharedNode.kt` opens it through
that package's context (`SharedNode.open(context, app, hostPackage =
...)`), falls back to a private node with a warning when the host does
not share the uid, and wires `ProcessLifecycleOwner`: stop suspends
(handing the lease over), start resumes. There is no kill-on-lock rule
on Android, so a foreground Service (`dataSync`) may keep serving in
the background by setting `keepServingInBackground`. Uninstalling the
host package deletes the node. Sharing across vendors on Android needs
a host app plus IPC and is not covered here.

## 5. Apps in the node

An app opens the node with its `AppInfo` (`Node.open(dir, offline, app,
policy)` in Swift/Kotlin, `Node(dir, app_id=...)` in Python, the CLI
registers itself as `network.resonator.rsntr`) and appears in `_apps`:
id, name, version, library version, platform, first and last seen. It
declares the tables it keeps there (`declareTables`, `rsntr apps
declare`): each gets its `_comments` description (mandatory: a database
a human can read starts with knowing what each table is for) and the
ownership record. Conventions, not laws: prefix your tables with your
app's slug (`notebook_pages` for `org.example.notebook`); never touch
reserved tables (`_` prefix) or the chat scaffold; create the table
before declaring it. Every declared table is live for every other app's
entrainments and for `rsntr await`.

## 6. Finding what is here

Open the shared directory and ask the node itself:

    rsntr apps list -d <dir>          # who is here and what each declares
    rsntr system graph -d <dir>       # everything about the node as RDF (owner: unfiltered)
    rsntr sql <dir> "SELECT name, comment FROM _comments"

In the system graph every app is an `rsntr:App` (`urn:rsntr:app:<id>`)
the node `rsntr:hosts`, and every declared table an `rsntr:Table` it
`rsntr:declares`, with the comment as `rdfs:comment`
(`docs/rsntr-system.ttl`). Then use the tables with SQL, entrain a point
whose `resource` is a table that interests you (or create one with a
`_projection` row), and declare your own.

## 7. Inviting peers

Keep the user's information on the user's device, in the one node every
app there shares; when it should travel, invite the peer through
resonator: a `_policy` row and a `_projection` point, revocable,
audited, never a copy you cannot recall. No new mechanism: a peer sees
a table when `_policy` grants it `read`, changes it when `write` is
granted, entrains it through a Sympathetic point, and finds all of it
in the projection and the system graph, which name only what the caller
may use.

## 8. Migrating a per-app node

Move the directory (key and all) to the shared location and open it
from there; the identity travels with the key. Never copy a directory
to a second device.

## 9. What changed for existing directories

- New files: `rsntr.serve.lock`/`rsntr.serve.json`, `rsntr.owner`
  (while serving), `rsntr.init.lock`, `blobs-inbox/` (transient).
- New table `_apps`; `_rsntr` gains `schema_version` and
  `min_reader_version` (an older build refuses a database a newer build
  migrated past what it can read; every change so far is additive).
- `synchronous = NORMAL` in WAL (crash-consistent; the last commits may
  be lost on power loss, never the database) and a bounded WAL.
- Write transactions are immediate; `open` retries on busy.
- `chat wait` and `chat watch` ride the owner channel and no longer
  admit an ephemeral identity.
- Client endpoints (`query`, `knock`, `fetch`, `ticket`) no longer
  publish the node's address record; only the serving endpoint does.
- `rsntr` without `-d` uses the resolution in 4.1 instead of `.`.
