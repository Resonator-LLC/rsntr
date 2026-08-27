//! The `rsntr` binary: argument parsing and printing over the library in
//! `lib.rs`.
//!
//! Exit codes (stable): 0 ok, 1 error, 2 denied, 3 timeout. With the
//! global `--json` flag every command emits stable machine-readable JSON
//! on stdout (one object per command; `entrain` emits one object per
//! event line; `watch` keeps raw bytes on stdout and reports on stderr).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use serde_json::json;

use rsntr::{
    EXIT_CODES_HELP, EXIT_DENIED, EXIT_ERROR, EXIT_OK, client, output, serve, store, teletype,
};

#[derive(Parser)]
#[command(
    name = "rsntr",
    about = "resonator: p2p SQL and SPARQL over an RDF envelope",
    version,
    disable_help_subcommand = true,
    after_help = EXIT_CODES_HELP
)]
struct Cli {
    /// Emit stable machine-readable JSON on stdout.
    #[arg(long, global = true)]
    json: bool,
    /// Owner channel: require the serving node's control socket; fail
    /// when no node is serving (docs/owner-channel.md sec 3.3).
    #[arg(long, global = true, conflicts_with = "local")]
    socket: bool,
    /// Owner channel: force the in-process transport even beside a
    /// serving node (writes are then not live in the serving process).
    #[arg(long, global = true)]
    local: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a node directory: database, key file, _ tables, defaults.
    Init {
        /// Directory to initialize (created if missing).
        dir: PathBuf,
    },
    /// Start the serving node as a daemon (log in <dir>/rsntr.log) and
    /// print its summary; --foreground runs it in the terminal instead.
    /// An uninitialized directory is initialized first.
    Serve {
        /// Node directory (from `rsntr init`; created when missing).
        dir: PathBuf,
        /// Bind localhost only, no relays, no address lookup services.
        #[arg(long)]
        offline: bool,
        /// Also serve the web interface (docs/web-api.md); optionally
        /// takes the bind address (default 127.0.0.1:2718). Prints the
        /// entry URL with the capability token in the fragment.
        #[arg(
            long,
            value_name = "ADDR",
            num_args = 0..=1,
            default_missing_value = rsntr::serve::DEFAULT_WEB_ADDR
        )]
        web: Option<SocketAddr>,
        /// Rotate the persisted web capability token (rsntr.web-token)
        /// before serving; signed-in browsers must re-enter it.
        #[arg(long, requires = "web")]
        new_web_token: bool,
        /// Run attached in this terminal until ctrl-c (the pre-0.2
        /// default; systemd units and debugging).
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the serving daemon of a node directory (SIGTERM, then
    /// SIGKILL after 10s). Idempotent.
    Stop {
        /// Node directory.
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Report whether a node directory is being served, plus its
    /// endpoint id, addresses, ticket, peer count, and pending inbox.
    Status {
        /// Node directory.
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Event hooks: commands the serving daemon runs when something new
    /// arrives (a chat message, an inbox knock), JSON event on stdin.
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
    /// Run SPARQL on this node (owner channel) or on a peer (--peer).
    /// SELECT/ASK print rows; CONSTRUCT/DESCRIBE print triples; updates
    /// (INSERT DATA etc.) ride an Execute.
    Sparql(SparqlArgs),
    /// Load or dump RDF as Turtle, locally or against a peer.
    Turtle {
        #[command(subcommand)]
        command: TurtleCommand,
    },
    /// Named binary streams between nodes (duplex byte pipes over the
    /// media machinery; gated by the `media` policy action).
    #[cfg(unix)]
    Pipe {
        #[command(subcommand)]
        command: PipeCommand,
    },
    /// Internal: the serving-side bridge command `pipe accept`
    /// registers. Not for direct use.
    #[cfg(unix)]
    #[command(hide = true, name = "pipe-bridge")]
    PipeBridge {
        /// The accept process's unix socket.
        #[arg(value_name = "SOCKET")]
        bridge_socket: PathBuf,
    },
    /// Print this node's endpoint id.
    Id {
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Print a dialing ticket from a short-lived endpoint. Prefer the
    /// live ticket `rsntr serve` prints while the node is up.
    Ticket {
        /// Node directory.
        dir: PathBuf,
        /// Direct addresses only, no relay (local-network use).
        #[arg(long)]
        offline: bool,
    },
    /// Peer registry commands.
    Peer {
        #[command(subcommand)]
        command: PeerCommand,
    },
    /// Media source registry commands (the `media` modulation).
    Media {
        #[command(subcommand)]
        command: MediaCommand,
    },
    /// Wasm mod registry commands (the extism mods host).
    #[cfg(feature = "mods")]
    Mod {
        #[command(subcommand)]
        command: ModCommand,
    },
    /// Knock on a peer that has not admitted this node
    /// (connection-protocol.md sec 4): sends the one frame a stranger
    /// may send; the answer is allow, deny, or parked for the owner.
    Knock(KnockArgs),
    /// Send one statement to a peer and print the result.
    Query(QueryArgs),
    /// Ask a peer for usage guidance (the `help` modulation).
    Help(HelpArgs),
    /// Fetch a peer's capability menu (the `projection` modulation) and
    /// render it teletype-style; optionally choose and invoke a point.
    Projection(ProjectionArgs),
    /// Entrain to a Sympathetic point and print each vibration; ctrl-c
    /// damps the entrainment.
    Entrain(EntrainArgs),
    /// Open a peer's media source: header to stderr, raw feed to stdout
    /// (e.g. `rsntr watch station nvr/39 | ffplay -f mpegts -`).
    Watch(WatchArgs),
    /// Open a peer's audio-duplex source: stdin goes up the wire in the
    /// source's accepts format, the downstream feed comes out on stdout
    /// (e.g. `sox -d -t raw -r 8000 -e signed -b 16 -c 1 - | rsntr talk
    /// station door-talk`).
    Talk(TalkArgs),
    /// CSV export/import against the local node, sharing the web
    /// interface's codec and pipeline path.
    Csv {
        #[command(subcommand)]
        command: CsvCommand,
    },
    /// Run SQL on this node over the owner channel (DDL allowed): one
    /// statement, or a --file of ';'-separated statements such as an
    /// example mod's seed.sql.
    Sql {
        /// Node directory.
        dir: PathBuf,
        /// One SQL statement.
        stmt: Option<String>,
        /// File of ';'-separated statements (no semicolons inside
        /// literals or comments).
        #[arg(long, conflicts_with = "stmt")]
        file: Option<PathBuf>,
    },
    /// Chat: direct and room messages over the `chat` modulation
    /// (docs/chat-protocol.md).
    Chat {
        #[command(subcommand)]
        command: ChatCommand,
    },
    /// The `_inbox` parking table: requests awaiting the owner's
    /// decision (knocks and human-tier escalations).
    Inbox {
        #[command(subcommand)]
        command: InboxCommand,
    },
    /// Fetch a blob by BLAKE3 hash from a peer over iroh-blobs, verified
    /// streaming; bytes to -o <path> or stdout.
    Fetch(FetchArgs),
    /// The built-in operating manual: node lifecycle, pairing, chat,
    /// hooks, RDF, pipes — everything an agent or human needs, offline.
    Guide {
        /// A topic (e.g. lifecycle, pairing, chat, hooks, rdf, pipes,
        /// security, intro); the whole manual when omitted.
        topic: Option<String>,
    },
}

#[derive(Subcommand)]
enum HookCommand {
    /// Register a hook: run <command> via `sh -c` on each <event>
    /// ("message", "inbox", or "*"), one JSON event on stdin.
    Add {
        /// The event: message, inbox, or *.
        event: String,
        /// The command, run with `sh -c` by the serving daemon.
        command: String,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// List registered hooks.
    List {
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Remove a hook by id.
    Rm {
        id: i64,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Enable a hook by id.
    Enable {
        id: i64,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Disable a hook by id.
    Disable {
        id: i64,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(Args)]
struct SparqlArgs {
    /// The SPARQL text (or use --file).
    query: Option<String>,
    /// Read the SPARQL text from a file.
    #[arg(long, conflicts_with = "query")]
    file: Option<PathBuf>,
    /// Run against this peer (petname or 64-hex endpoint id) instead of
    /// the own node; reads need `read` policy there, updates `write`.
    #[arg(long)]
    peer: Option<String>,
    /// Client-side timeout hint in milliseconds.
    #[arg(long)]
    timeout_ms: Option<i64>,
    /// Node directory.
    #[arg(short, long, default_value = ".")]
    dir: PathBuf,
    /// Dial without relays or lookup services (requires stored addrs).
    #[arg(long)]
    offline: bool,
}

#[derive(Subcommand)]
enum TurtleCommand {
    /// Load a Turtle document into the RDF store, as chunked idempotent
    /// INSERT DATA updates.
    Load {
        /// The Turtle file.
        file: PathBuf,
        /// Load into this peer's store (needs `write` policy there).
        #[arg(long)]
        peer: Option<String>,
        /// INSERT DATA chunk budget in bytes of triple text.
        #[arg(long, default_value_t = rsntr::rdfcmd::DEFAULT_CHUNK_BYTES)]
        chunk_bytes: usize,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
        /// Dial without relays or lookup services (requires stored addrs).
        #[arg(long)]
        offline: bool,
    },
    /// Dump the RDF store as triples (CONSTRUCT WHERE { ?s ?p ?o },
    /// budgeted; big graphs report truncated).
    Dump {
        /// Dump this peer's store (needs `read` policy there).
        #[arg(long)]
        peer: Option<String>,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
        /// Dial without relays or lookup services (requires stored addrs).
        #[arg(long)]
        offline: bool,
    },
}

#[cfg(unix)]
#[derive(Subcommand)]
enum PipeCommand {
    /// Register a pipe endpoint: <command> runs per connection, its
    /// stdin is the caller's bytes, its stdout the reply stream.
    Add {
        /// Endpoint name, as dialed by `pipe open`.
        name: String,
        /// Command run with `sh -c` by the serving daemon.
        command: String,
        /// Watch-only endpoint (stdout feed, no caller bytes).
        #[arg(long)]
        one_way: bool,
        /// Optional note.
        #[arg(long)]
        note: Option<String>,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// List registered pipe endpoints.
    List {
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Remove a pipe endpoint.
    Rm {
        name: String,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Open a peer's pipe: stdin goes up the wire, the reply stream
    /// comes out on stdout.
    Open {
        /// Target peer: a petname from `_peers` or a 64-hex endpoint id.
        peer: String,
        /// The pipe name (as registered on the peer).
        name: String,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
        /// Dial without relays or lookup services (requires stored addrs).
        #[arg(long)]
        offline: bool,
    },
    /// Wait for one incoming connection on <name> and bridge it to this
    /// terminal's stdin/stdout (no pre-registered command needed).
    Accept {
        /// The pipe name callers will open.
        name: String,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum ChatCommand {
    /// Scaffold chat in a node directory: tables, projection points,
    /// policy rows, mods entry. Idempotent.
    Init {
        /// Node directory (from `rsntr init`).
        dir: PathBuf,
    },
    /// Append locally and enqueue for delivery (offline peers get it on
    /// reconnection). Prints the message id.
    Send {
        /// A peer (petname or 64-hex endpoint id) or a room (name or IRI).
        target: String,
        /// The message text (or use --body-file; big bodies auto-spill).
        text: Option<String>,
        /// Read the message body from a file, `-` for stdin — the path
        /// past the OS's argument size cap for very large bodies.
        #[arg(long, conflicts_with = "text")]
        body_file: Option<PathBuf>,
        /// Attach a file: imported into the local blob store, sent as a
        /// BlobRef (bytes fetched out of band).
        #[arg(long)]
        file: Option<PathBuf>,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Local read of the message log for one peer or room, newest
    /// first, with delivery status for own messages.
    Log {
        /// A peer (petname or 64-hex endpoint id) or a room (name or IRI).
        target: String,
        /// Maximum messages to print.
        #[arg(long, default_value_t = 50)]
        limit: i64,
        /// Only messages after this id (a message ULID) — the agent's
        /// cursor for incremental reads.
        #[arg(long)]
        since: Option<String>,
        /// Keep spilled big bodies as preview + blobref instead of
        /// fetching and inlining the full text.
        #[arg(long)]
        no_inline: bool,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Block until an incoming message arrives (for one target, or any
    /// when omitted) or the timeout elapses: one JSON object out, exit 0
    /// on message, 3 on timeout. Needs a serving daemon.
    Wait {
        /// A peer (petname or 64-hex endpoint id) or a room (name or
        /// IRI); every scope when omitted.
        target: Option<String>,
        /// Give up after this many seconds.
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        /// Keep spilled big bodies as preview + blobref.
        #[arg(long)]
        no_inline: bool,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Entrain the local inbox point (self-dial the serving node) and
    /// print messages for that scope as they land; ctrl-c stops.
    Watch {
        /// A peer (petname or 64-hex endpoint id) or a room (name or IRI).
        target: String,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Room commands (creator-hosted rooms).
    Room {
        #[command(subcommand)]
        command: RoomCommand,
    },
}

#[derive(Subcommand)]
enum RoomCommand {
    /// Mint a room IRI hosted by this node. Prints the IRI.
    Create {
        /// Display name (not unique).
        name: String,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Host only: admit an already-admitted peer to a room.
    Add {
        /// The room (name or IRI).
        room: String,
        /// The peer (petname or 64-hex endpoint id), already in _peers.
        peer: String,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Member side: record <peer> as the host of <room-iri> locally.
    Join {
        /// The host peer (petname or 64-hex endpoint id).
        peer: String,
        /// The room IRI (urn:rsntr:room:<ULID>).
        room_iri: String,
        /// Local display name for the room.
        #[arg(long)]
        name: Option<String>,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum InboxCommand {
    /// List pending `_inbox` rows (all rows with --all).
    List {
        /// Include answered rows too.
        #[arg(long)]
        all: bool,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Answer a pending row: a statement answer feeds the decision
    /// cache; allowing a knock admits the peer.
    Answer {
        /// The row's request id (from `rsntr inbox list`).
        id: String,
        /// The decision.
        #[arg(value_parser = ["allow", "deny"])]
        verdict: String,
        /// Also write a generated `_policy` row per footprint table
        /// (allow-and-remember), so this shape never asks again.
        #[arg(long)]
        remember: bool,
        /// On a knock allow: also grant the peer `_policy` rows,
        /// `table=action` each (e.g. --grant 'door=media'
        /// --grant '*=mod:cameras'). Admission alone grants nothing.
        #[arg(long = "grant", value_name = "TABLE=ACTION")]
        grants: Vec<String>,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(Args)]
struct FetchArgs {
    /// Provider peer: a petname from `_peers` or a 64-hex endpoint id.
    peer: String,
    /// The blob hash: 64-hex BLAKE3, `blake3:` prefix accepted.
    hash: String,
    /// Write the bytes here; stdout when omitted.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Node directory.
    #[arg(short, long, default_value = ".")]
    dir: PathBuf,
    /// Dial without relays or lookup services (requires stored addrs).
    #[arg(long)]
    offline: bool,
}

#[derive(Subcommand)]
enum CsvCommand {
    /// Export a table as RFC 4180 CSV (to a file, or stdout).
    Export {
        /// The table to export.
        table: String,
        /// Output file; stdout when omitted.
        file: Option<PathBuf>,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Import RFC 4180 CSV into a table (header line required; the
    /// header must match the table's columns).
    Import {
        /// The target table.
        table: String,
        /// The CSV file to import.
        file: PathBuf,
        /// Create the table (one TEXT column per header field) when it
        /// does not exist.
        #[arg(long)]
        create: bool,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum PeerCommand {
    /// Admit a peer: record its endpoint id or ticket (plus optional
    /// dial addresses).
    Add {
        /// Local petname for the peer.
        name: String,
        /// The peer's 64-hex endpoint id, or a dialing ticket.
        target: String,
        /// Optional direct socket addresses, e.g. 127.0.0.1:4433.
        addrs: Vec<SocketAddr>,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum MediaCommand {
    /// Register (or update) a media source: its command's stdout is the feed.
    Add {
        /// Node directory.
        dir: PathBuf,
        /// Source name, as requested in rsntr:signal (e.g. nvr/39).
        name: String,
        /// Command run with `sh -c`; stdout must be the media byte stream.
        command: String,
        /// Media type of the stream.
        #[arg(long, default_value = "video/mp2t")]
        content_type: String,
        /// Media type the command's stdin accepts; set to make this an
        /// audio-duplex source (e.g. "audio/L16;rate=8000;channels=1").
        #[arg(long)]
        accepts: Option<String>,
        /// Optional note.
        #[arg(long)]
        note: Option<String>,
    },
    /// List registered media sources.
    List {
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Allow a peer (petname, endpoint id, or *) to open a source (name or *).
    Allow {
        /// The peer: a petname from `_peers`, a 64-hex endpoint id, or `*`.
        peer: String,
        /// The source name from `_media`, or `*` for all sources.
        name: String,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
}

#[cfg(feature = "mods")]
#[derive(Subcommand)]
enum ModCommand {
    /// Register (or replace) a mod from a .wasm file; starts disabled.
    Add {
        /// Modulation name the mod serves (must match its descriptor).
        name: String,
        /// The plugin wasm file.
        file: PathBuf,
        /// Grant a capability (db_read, db_write, clock); repeatable.
        #[arg(long = "cap")]
        caps: Vec<String>,
        /// Optional note.
        #[arg(long)]
        note: Option<String>,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// List registered mods.
    List {
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Enable a mod (takes effect at the next `rsntr serve` start).
    Enable {
        name: String,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Disable a mod.
    Disable {
        name: String,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Remove a mod row.
    Rm {
        name: String,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Load the stored wasm and print its descriptor.
    Describe {
        name: String,
        /// Node directory.
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(Args)]
struct KnockArgs {
    /// Target peer: a petname from `_peers` or a 64-hex endpoint id.
    peer: String,
    /// Who you are and what you want; lands in the owner's inbox.
    message: String,
    /// Node directory.
    #[arg(short, long, default_value = ".")]
    dir: PathBuf,
    /// Dial without relays or lookup services (requires stored addrs).
    #[arg(long)]
    offline: bool,
}

#[derive(Args)]
struct QueryArgs {
    /// Target peer: a petname from `_peers` or a 64-hex endpoint id.
    peer: String,
    /// The statement text (SQL with ? placeholders, or SPARQL).
    signal: String,
    /// The modulation carrying the statement.
    #[arg(long = "mod", default_value = "sql-sqlite")]
    modulation: String,
    /// Positional parameter values (text), one flag per parameter.
    #[arg(long = "param")]
    params: Vec<String>,
    /// Client-side timeout hint in milliseconds.
    #[arg(long)]
    timeout_ms: Option<i64>,
    /// Node directory.
    #[arg(short, long, default_value = ".")]
    dir: PathBuf,
    /// Dial without relays or lookup services (requires stored addrs).
    #[arg(long)]
    offline: bool,
}

#[derive(Args)]
struct HelpArgs {
    /// Target peer: a petname from `_peers` or a 64-hex endpoint id.
    peer: String,
    /// Optional drill-down topic (e.g. modulations, tables, knock).
    topic: Option<String>,
    /// Node directory.
    #[arg(short, long, default_value = ".")]
    dir: PathBuf,
    /// Dial without relays or lookup services (requires stored addrs).
    #[arg(long)]
    offline: bool,
}

#[derive(Args)]
struct ProjectionArgs {
    /// Target peer: a petname from `_peers` or a 64-hex endpoint id.
    peer: String,
    /// Projection path (as handed out by a point's rsntr:projects);
    /// empty is the root.
    #[arg(default_value = "")]
    path: String,
    /// Choose menu entry N: zoom into it, or invoke it with the --field
    /// values (prompting for missing fields on a terminal).
    #[arg(long)]
    choose: Option<usize>,
    /// A field value for the chosen point, as name=value; repeatable.
    #[arg(long = "field")]
    fields: Vec<String>,
    /// Node directory.
    #[arg(short, long, default_value = ".")]
    dir: PathBuf,
    /// Dial without relays or lookup services (requires stored addrs).
    #[arg(long)]
    offline: bool,
}

#[derive(Args)]
struct EntrainArgs {
    /// Target peer: a petname from `_peers` or a 64-hex endpoint id.
    peer: String,
    /// The Sympathetic point's IRI (from the peer's projection).
    point: String,
    /// Node directory.
    #[arg(short, long, default_value = ".")]
    dir: PathBuf,
    /// Dial without relays or lookup services (requires stored addrs).
    #[arg(long)]
    offline: bool,
}

#[derive(Args)]
struct WatchArgs {
    /// Target peer: a petname from `_peers` or a 64-hex endpoint id.
    peer: String,
    /// The media source name (as registered on the peer, e.g. nvr/39).
    source: String,
    /// Node directory.
    #[arg(short, long, default_value = ".")]
    dir: PathBuf,
    /// Dial without relays or lookup services (requires stored addrs).
    #[arg(long)]
    offline: bool,
}

#[derive(Args)]
struct TalkArgs {
    /// Target peer: a petname from `_peers` or a 64-hex endpoint id.
    peer: String,
    /// The audio-duplex source name (as registered on the peer).
    source: String,
    /// Node directory.
    #[arg(short, long, default_value = ".")]
    dir: PathBuf,
    /// Dial without relays or lookup services (requires stored addrs).
    #[arg(long)]
    offline: bool,
}

fn main() -> ExitCode {
    // Default: warnings only, none of iroh's shutdown-path noise;
    // RUST_LOG overrides for debugging. Logs go to stderr; stdout is data.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,iroh=off"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    // clap's default exit code for usage errors is 2, which this CLI
    // reserves for "denied"; usage errors must exit 1 instead.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            use clap::error::ErrorKind;
            let code = match e.kind() {
                ErrorKind::DisplayHelp
                | ErrorKind::DisplayVersion
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => EXIT_OK,
                _ => EXIT_ERROR,
            };
            e.print().expect("writing the clap message");
            return ExitCode::from(code.clamp(0, 255) as u8);
        }
    };
    let json = cli.json;
    let prefer = if cli.socket {
        rsntr::Prefer::Socket
    } else if cli.local {
        rsntr::Prefer::Local
    } else {
        rsntr::Prefer::Auto
    };
    let code = match run(cli.command, json, prefer) {
        Ok(code) => code,
        Err(e) => {
            if json {
                println!(
                    "{}",
                    json!({ "ok": false, "error": { "code": "cli", "reason": format!("{e:#}") } })
                );
            } else {
                eprintln!("error: {e:#}");
            }
            EXIT_ERROR
        }
    };
    ExitCode::from(code.clamp(0, 255) as u8)
}

fn run(command: Command, json: bool, prefer: rsntr::Prefer) -> Result<i32> {
    match command {
        Command::Init { dir } => {
            let id = store::init_dir(&dir)?;
            // Chat is on by default: a fresh node gets the scaffold
            // (tables, projection points, policy) so the console and
            // `rsntr chat` work out of the box. `rsntr chat init`
            // remains for directories created before this default.
            let chat = rsntr::chat::chat_init_with(&dir, prefer)
                .map(|_| true)
                .unwrap_or_else(|e| {
                    eprintln!("warning: chat scaffold failed: {e:#}");
                    false
                });
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "dir": dir.display().to_string(),
                        "endpoint_id": id.to_string(),
                        "chat": chat,
                    })
                );
            } else {
                println!("initialized {}", dir.display());
                println!("endpoint id: {id}");
                if chat {
                    println!("chat: scaffolded (tables, points, policy)");
                }
            }
            Ok(EXIT_OK)
        }
        Command::Id { dir } => {
            let id = store::node_id(&dir)?;
            if json {
                println!("{}", json!({ "ok": true, "endpoint_id": id.to_string() }));
            } else {
                println!("{id}");
            }
            Ok(EXIT_OK)
        }
        Command::Peer {
            command:
                PeerCommand::Add {
                    name,
                    target,
                    addrs,
                    dir,
                },
        } => {
            let (id, stored) = store::peer_add_with(&dir, prefer, &name, &target, &addrs)?;
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "name": name,
                        "endpoint_id": id.to_string(),
                        "addrs": stored.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
                    })
                );
            } else {
                println!("added peer {name} ({id})");
            }
            Ok(EXIT_OK)
        }
        Command::Media { command } => match command {
            MediaCommand::Add {
                dir,
                name,
                command,
                content_type,
                accepts,
                note,
            } => {
                store::media_add_duplex(
                    &dir,
                    prefer,
                    &name,
                    &command,
                    &content_type,
                    accepts.as_deref(),
                    note.as_deref(),
                )?;
                if json {
                    println!("{}", json!({ "ok": true, "name": name }));
                } else {
                    println!("media source {name} registered");
                }
                Ok(EXIT_OK)
            }
            MediaCommand::List { dir } => {
                let sources = store::media_list_with(&dir, prefer)?;
                if json {
                    println!(
                        "{}",
                        json!({
                            "ok": true,
                            "sources": sources
                                .iter()
                                .map(|(name, content_type, command)| json!({
                                    "name": name,
                                    "content_type": content_type,
                                    "command": command,
                                }))
                                .collect::<Vec<_>>(),
                        })
                    );
                } else {
                    for (name, content_type, command) in sources {
                        println!("{name}\t{content_type}\t{command}");
                    }
                }
                Ok(EXIT_OK)
            }
            MediaCommand::Allow { peer, name, dir } => {
                let peer_id = store::media_allow_with(&dir, prefer, &peer, &name)?;
                if json {
                    println!("{}", json!({ "ok": true, "peer": peer_id, "source": name }));
                } else {
                    println!("policy added: {peer_id} may open media source {name}");
                }
                Ok(EXIT_OK)
            }
        },
        #[cfg(feature = "mods")]
        Command::Mod { command } => cmd_mod(command, json, prefer),
        Command::Ticket { dir, offline } => runtime()?.block_on(cmd_ticket(dir, offline, json)),
        Command::Serve {
            dir,
            offline,
            web,
            new_web_token,
            foreground,
        } => {
            // Auto-init: one command bootstraps an agent node (the chat
            // scaffold matches `rsntr init`).
            if !store::db_path(&dir).exists() {
                let id = store::init_dir(&dir)?;
                if let Err(e) = rsntr::chat::chat_init_with(&dir, prefer) {
                    eprintln!("warning: chat scaffold failed: {e:#}");
                }
                eprintln!("initialized {} (endpoint id {id})", dir.display());
            }
            if foreground {
                runtime()?.block_on(cmd_serve(dir, offline, web, new_web_token, json))
            } else {
                #[cfg(unix)]
                {
                    runtime()?.block_on(cmd_serve_detached(dir, offline, web, new_web_token, json))
                }
                #[cfg(not(unix))]
                {
                    eprintln!("detached serving needs unix; running in the foreground");
                    runtime()?.block_on(cmd_serve(dir, offline, web, new_web_token, json))
                }
            }
        }
        #[cfg(unix)]
        Command::Stop { dir } => runtime()?.block_on(cmd_stop(dir, json)),
        #[cfg(unix)]
        Command::Status { dir } => runtime()?.block_on(cmd_status(dir, json)),
        #[cfg(not(unix))]
        Command::Stop { .. } | Command::Status { .. } => {
            anyhow::bail!("this command needs unix (the control socket)")
        }
        Command::Hook { command } => runtime()?.block_on(cmd_hook(command, json, prefer)),
        Command::Sparql(args) => runtime()?.block_on(cmd_sparql(args, json, prefer)),
        Command::Turtle { command } => runtime()?.block_on(cmd_turtle(command, json, prefer)),
        #[cfg(unix)]
        Command::Pipe { command } => runtime()?.block_on(cmd_pipe(command, json, prefer)),
        #[cfg(unix)]
        Command::PipeBridge { bridge_socket } => {
            runtime()?.block_on(rsntr::pipecmd::pipe_bridge(&bridge_socket))?;
            Ok(EXIT_OK)
        }
        Command::Csv { command } => runtime()?.block_on(cmd_csv(command, json, prefer)),
        Command::Sql { dir, stmt, file } => cmd_sql(dir, stmt, file, json, prefer),
        Command::Chat { command } => match command {
            ChatCommand::Init { dir } => {
                let id = rsntr::chat::chat_init_with(&dir, prefer)?;
                if json {
                    println!(
                        "{}",
                        json!({
                            "ok": true,
                            "dir": dir.display().to_string(),
                            "endpoint_id": id.to_string(),
                        })
                    );
                } else {
                    println!("chat scaffolded in {}", dir.display());
                }
                Ok(EXIT_OK)
            }
            ChatCommand::Send {
                target,
                text,
                body_file,
                file,
                dir,
            } => {
                let text = match (text, body_file) {
                    (Some(t), None) => t,
                    (None, Some(p)) if p.as_os_str() == "-" => {
                        use std::io::Read;
                        let mut buf = String::new();
                        std::io::stdin()
                            .read_to_string(&mut buf)
                            .context("reading the body from stdin")?;
                        buf
                    }
                    (None, Some(p)) => std::fs::read_to_string(&p)
                        .with_context(|| format!("reading {}", p.display()))?,
                    (None, None) => anyhow::bail!("give a message text or --body-file <path>"),
                    (Some(_), Some(_)) => unreachable!("clap conflicts_with"),
                };
                runtime()?.block_on(cmd_chat_send(dir, target, text, file, json, prefer))
            }
            ChatCommand::Log {
                target,
                limit,
                since,
                no_inline,
                dir,
            } => runtime()?.block_on(cmd_chat_log(
                &dir,
                &target,
                limit,
                since.as_deref(),
                no_inline,
                json,
            )),
            ChatCommand::Wait {
                target,
                timeout,
                no_inline,
                dir,
            } => runtime()?.block_on(cmd_chat_wait(dir, target, timeout, no_inline, json)),
            ChatCommand::Watch { target, dir } => {
                runtime()?.block_on(cmd_chat_watch(dir, target, json))
            }
            ChatCommand::Room { command } => cmd_chat_room(command, json, prefer),
        },
        Command::Inbox { command } => runtime()?.block_on(cmd_inbox(command, json, prefer)),
        Command::Fetch(args) => runtime()?.block_on(cmd_fetch(args, json)),
        Command::Guide { topic } => cmd_guide(topic.as_deref(), json),
        Command::Knock(args) => runtime()?.block_on(cmd_knock(args, json)),
        Command::Query(args) => runtime()?.block_on(cmd_query(args, json)),
        Command::Help(args) => runtime()?.block_on(cmd_help(args, json)),
        Command::Projection(args) => runtime()?.block_on(cmd_projection(args, json)),
        Command::Entrain(args) => runtime()?.block_on(cmd_entrain(args, json)),
        Command::Watch(args) => runtime()?.block_on(cmd_watch(args, json)),
        Command::Talk(args) => runtime()?.block_on(cmd_talk(args, json)),
    }
}

async fn cmd_talk(args: TalkArgs, json: bool) -> Result<i32> {
    match client::run_talk(&args.dir, &args.peer, &args.source, args.offline).await? {
        client::TalkOutcome::Done => {
            if json {
                println!("{}", json!({ "ok": true }));
            } else {
                eprintln!("talk session ended");
            }
            Ok(EXIT_OK)
        }
        client::TalkOutcome::Denied(d) => {
            let reason = d.reason.unwrap_or_default();
            if json {
                println!(
                    "{}",
                    json!({ "ok": false, "error": { "code": "denied", "reason": reason } })
                );
            } else {
                eprintln!("denied: {reason}");
            }
            Ok(EXIT_DENIED)
        }
        client::TalkOutcome::Failed(e) => {
            let reason = e.reason.unwrap_or_default();
            if json {
                println!(
                    "{}",
                    json!({ "ok": false, "error": { "code": e.code, "reason": reason } })
                );
            } else {
                eprintln!("error [{}] {reason}", e.code);
            }
            Ok(EXIT_ERROR)
        }
    }
}

#[cfg(feature = "mods")]
fn cmd_mod_toggle(
    dir: &std::path::Path,
    name: &str,
    enable: bool,
    json: bool,
    prefer: rsntr::Prefer,
) -> Result<i32> {
    if !rsntr::modcmd::mod_set_enabled(dir, prefer, name, enable)? {
        anyhow::bail!("no mod named {name:?}");
    }
    let state = if enable { "enabled" } else { "disabled" };
    if json {
        println!("{}", json!({ "ok": true, "name": name, "enabled": enable }));
    } else {
        println!("mod {name} {state} (takes effect at the next serve start)");
    }
    Ok(EXIT_OK)
}

#[cfg(feature = "mods")]
fn cmd_mod(command: ModCommand, json: bool, prefer: rsntr::Prefer) -> Result<i32> {
    match command {
        ModCommand::Add {
            dir,
            name,
            file,
            caps,
            note,
        } => {
            let wasm =
                std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let sha = rsntr::modcmd::mod_add(&dir, prefer, &name, &wasm, &caps, note.as_deref())?;
            if json {
                println!(
                    "{}",
                    json!({ "ok": true, "name": name, "sha256": sha, "caps": caps })
                );
            } else {
                println!("mod {name} registered (sha256 {sha}), disabled");
                println!(
                    "enable it with: rsntr mod enable {name} -d {}",
                    dir.display()
                );
            }
            Ok(EXIT_OK)
        }
        ModCommand::List { dir } => {
            let rows = rsntr::modcmd::mod_list(&dir, prefer)?;
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "mods": rows.iter().map(|m| json!({
                            "name": m.name,
                            "enabled": m.enabled,
                            "sha256": m.sha256,
                            "caps": m.caps,
                            "note": m.note,
                        })).collect::<Vec<_>>(),
                    })
                );
            } else {
                for m in rows {
                    println!(
                        "{}\t{}\t{}\t[{}]",
                        m.name,
                        if m.enabled { "enabled" } else { "disabled" },
                        m.sha256,
                        m.caps.join(", "),
                    );
                }
            }
            Ok(EXIT_OK)
        }
        ModCommand::Enable { name, dir } => cmd_mod_toggle(&dir, &name, true, json, prefer),
        ModCommand::Disable { name, dir } => cmd_mod_toggle(&dir, &name, false, json, prefer),
        ModCommand::Rm { name, dir } => {
            if !rsntr::modcmd::mod_rm(&dir, prefer, &name)? {
                anyhow::bail!("no mod named {name:?}");
            }
            if json {
                println!("{}", json!({ "ok": true, "name": name, "removed": true }));
            } else {
                println!("mod {name} removed");
            }
            Ok(EXIT_OK)
        }
        ModCommand::Describe { name, dir } => {
            let timeout = resonator_node::NodeConfig::default().max_duration_ms;
            let d = rsntr::modcmd::describe(&dir, prefer, &name, timeout)?;
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "mod": {
                            "abi": d.abi,
                            "name": d.name,
                            "version": d.version,
                            "help_text": d.help_text,
                            "topics": d.topics,
                            "needs": d.needs,
                        }
                    })
                );
            } else {
                println!("name: {}", d.name);
                println!("version: {}", d.version);
                println!("abi: {}", d.abi);
                if !d.needs.is_empty() {
                    println!("needs: {}", d.needs.join(", "));
                }
                if !d.topics.is_empty() {
                    println!("topics: {}", d.topics.join(", "));
                }
                println!("{}", d.help_text);
            }
            Ok(EXIT_OK)
        }
    }
}

fn cmd_guide(topic: Option<&str>, json: bool) -> Result<i32> {
    let text = match topic {
        None => Some(rsntr::guide::GUIDE.to_string()),
        Some(t) => rsntr::guide::lookup(t),
    };
    match text {
        Some(text) => {
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "topic": topic,
                        "text": text,
                        "topics": rsntr::guide::topics(),
                    })
                );
            } else {
                print!("{text}");
            }
            Ok(EXIT_OK)
        }
        None => {
            let topics = rsntr::guide::topics();
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": false,
                        "error": { "code": "topic-unknown", "reason": topic },
                        "topics": topics,
                    })
                );
            } else {
                eprintln!("no topic matches {:?}; one of:", topic.unwrap_or_default());
                for t in topics {
                    eprintln!("  {t}");
                }
            }
            Ok(EXIT_ERROR)
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

async fn cmd_ticket(dir: PathBuf, offline: bool, json: bool) -> Result<i32> {
    let secret = store::load_secret(&dir)?;
    // No relay list here: this path mints a ticket without opening the
    // node, so it has nothing to read a configured relay from and takes
    // n0's. A ticket for a node serving on its own relays comes from
    // `RunningNode::ready_ticket`, off the live endpoint.
    let ticket =
        resonator_transport::mint_ticket(secret, offline, &[], std::time::Duration::from_secs(3))
            .await
            .map_err(|e| anyhow::anyhow!("minting the ticket: {e}"))?;
    if json {
        println!("{}", json!({ "ok": true, "ticket": ticket }));
    } else {
        println!("{ticket}");
    }
    Ok(EXIT_OK)
}

async fn cmd_serve(
    dir: PathBuf,
    offline: bool,
    web: Option<SocketAddr>,
    new_web_token: bool,
    json: bool,
) -> Result<i32> {
    let running = serve::start_node(&dir, offline).await?;
    let web_server = match web {
        Some(addr) => Some(serve::start_web(&running, &dir, addr, new_web_token).await?),
        None => None,
    };
    // The live ticket of the endpoint actually accepting connections:
    // same key, same port. (A minted `rsntr ticket` names a throwaway
    // endpoint, which is only right when nothing is serving.)
    let ticket = running
        .ready_ticket(std::time::Duration::from_secs(3))
        .await;
    // The daemon's own record: pid file and published ticket, read by
    // `rsntr stop`/`status` and the detach parent; removed on graceful
    // shutdown.
    #[cfg(unix)]
    {
        std::fs::write(
            rsntr::detach::pid_path(&dir),
            format!("{}\n", std::process::id()),
        )
        .with_context(|| "writing the pid file")?;
        let t = ticket.clone();
        running
            .node()
            .db()
            .call(move |conn| {
                resonator_node::set_rsntr(conn, rsntr::detach::SERVING_TICKET_KEY, &t)
            })
            .await
            .ok();
    }
    if json {
        let mut out = json!({
            "ok": true,
            "serving": dir.display().to_string(),
            "endpoint_id": running.peer_id().to_string(),
            "ticket": ticket,
            "addrs": running
                .direct_addrs()
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>(),
        });
        if let Some(ws) = &web_server {
            out["web"] = json!({
                "addr": ws.addr().to_string(),
                "url": ws.url(),
            });
        }
        println!("{out}");
    } else {
        println!("serving {}", dir.display());
        println!("endpoint id: {}", running.peer_id());
        println!("ticket: {ticket}");
        for addr in running.direct_addrs() {
            println!("listening on {addr}");
        }
        if let Some(ws) = &web_server {
            println!("web interface: {}", ws.url());
        }
        println!("ctrl-c to stop");
    }
    if let Some(ws) = &web_server
        && !ws.addr().ip().is_loopback()
    {
        eprintln!(
            "warning: the web interface is bound to a non-loopback address without TLS; \
             the bearer token travels in cleartext (front it with a TLS proxy)"
        );
    }
    // Ctrl-c or SIGTERM (`rsntr stop`) both shut down gracefully.
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            r = tokio::signal::ctrl_c() => r?,
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    if !json {
        eprintln!("shutting down");
    }
    if let Some(ws) = web_server {
        ws.shutdown().await;
    }
    #[cfg(unix)]
    {
        running
            .node()
            .db()
            .call(|conn| {
                conn.execute(
                    "DELETE FROM _rsntr WHERE key = ?1",
                    [rsntr::detach::SERVING_TICKET_KEY],
                )
            })
            .await
            .ok();
        let _ = std::fs::remove_file(rsntr::detach::pid_path(&dir));
    }
    running.shutdown().await;
    Ok(EXIT_OK)
}

#[cfg(unix)]
async fn cmd_serve_detached(
    dir: PathBuf,
    offline: bool,
    web: Option<SocketAddr>,
    new_web_token: bool,
    json: bool,
) -> Result<i32> {
    let report = rsntr::detach::serve_detached(&dir, offline, web, new_web_token).await?;
    if json {
        println!(
            "{}",
            json!({
                "ok": true,
                "already_running": report.already_running,
                "pid": report.pid,
                "socket": report.socket.display().to_string(),
                "endpoint_id": report.endpoint_id,
                "ticket": report.ticket,
                "log": rsntr::detach::log_path(&dir).display().to_string(),
            })
        );
    } else if report.already_running {
        println!(
            "already serving {} (pid {})",
            dir.display(),
            report
                .pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "unknown".into())
        );
    } else {
        println!(
            "serving {} (pid {}, log {})",
            dir.display(),
            report.pid.unwrap_or(0),
            rsntr::detach::log_path(&dir).display()
        );
        println!("endpoint id: {}", report.endpoint_id);
        if let Some(t) = &report.ticket {
            println!("ticket: {t}");
        }
        println!("stop it with: rsntr stop {}", dir.display());
    }
    Ok(EXIT_OK)
}

#[cfg(unix)]
async fn cmd_stop(dir: PathBuf, json: bool) -> Result<i32> {
    let report = rsntr::detach::stop(&dir).await?;
    if json {
        println!(
            "{}",
            json!({
                "ok": true,
                "stopped": report.stopped,
                "already_stopped": report.already_stopped,
                "pid": report.pid,
                "forced": report.forced,
            })
        );
    } else if report.already_stopped {
        println!("nothing is serving {}", dir.display());
    } else {
        println!(
            "stopped pid {}{}",
            report.pid.unwrap_or(0),
            if report.forced { " (forced)" } else { "" }
        );
    }
    Ok(EXIT_OK)
}

#[cfg(unix)]
async fn cmd_status(dir: PathBuf, json: bool) -> Result<i32> {
    let s = rsntr::detach::status(&dir).await?;
    if json {
        println!(
            "{}",
            json!({
                "ok": true,
                "serving": s.serving,
                "pid": s.pid,
                "endpoint_id": s.endpoint_id,
                "addrs": s.addrs,
                "ticket": s.ticket,
                "peers": s.peers,
                "pending_inbox": s.pending_inbox,
            })
        );
    } else {
        println!(
            "{}: {}",
            dir.display(),
            if s.serving {
                format!(
                    "serving (pid {})",
                    s.pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into())
                )
            } else {
                "not serving".to_string()
            }
        );
        println!("endpoint id: {}", s.endpoint_id);
        if let Some(t) = &s.ticket {
            println!("ticket: {t}");
        }
        println!("peers: {}, pending inbox: {}", s.peers, s.pending_inbox);
    }
    Ok(EXIT_OK)
}

async fn cmd_hook(command: HookCommand, json: bool, prefer: rsntr::Prefer) -> Result<i32> {
    match command {
        HookCommand::Add {
            event,
            command,
            dir,
        } => {
            let id = rsntr::hooks::hook_add(&dir, prefer, &event, &command).await?;
            if json {
                println!(
                    "{}",
                    json!({ "ok": true, "id": id, "event": event, "command": command })
                );
            } else {
                println!("hook {id} registered on {event}");
            }
            Ok(EXIT_OK)
        }
        HookCommand::List { dir } => {
            let rows = rsntr::hooks::hook_list(&dir, prefer).await?;
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "hooks": rows.iter().map(|h| json!({
                            "id": h.id,
                            "event": h.event,
                            "command": h.command,
                            "enabled": h.enabled,
                            "created_at": h.created_at,
                        })).collect::<Vec<_>>(),
                    })
                );
            } else {
                for h in rows {
                    println!(
                        "{}\t{}\t{}\t{}",
                        h.id,
                        h.event,
                        if h.enabled { "enabled" } else { "disabled" },
                        h.command,
                    );
                }
            }
            Ok(EXIT_OK)
        }
        HookCommand::Rm { id, dir } => {
            if !rsntr::hooks::hook_rm(&dir, prefer, id).await? {
                anyhow::bail!("no hook with id {id}");
            }
            if json {
                println!("{}", json!({ "ok": true, "id": id, "removed": true }));
            } else {
                println!("hook {id} removed");
            }
            Ok(EXIT_OK)
        }
        HookCommand::Enable { id, dir } => cmd_hook_toggle(&dir, id, true, json, prefer).await,
        HookCommand::Disable { id, dir } => cmd_hook_toggle(&dir, id, false, json, prefer).await,
    }
}

async fn cmd_hook_toggle(
    dir: &std::path::Path,
    id: i64,
    enabled: bool,
    json: bool,
    prefer: rsntr::Prefer,
) -> Result<i32> {
    if !rsntr::hooks::hook_set_enabled(dir, prefer, id, enabled).await? {
        anyhow::bail!("no hook with id {id}");
    }
    if json {
        println!("{}", json!({ "ok": true, "id": id, "enabled": enabled }));
    } else {
        println!("hook {id} {}", if enabled { "enabled" } else { "disabled" });
    }
    Ok(EXIT_OK)
}

async fn cmd_sparql(args: SparqlArgs, json: bool, prefer: rsntr::Prefer) -> Result<i32> {
    let text = match (args.query, args.file) {
        (Some(q), None) => q,
        (None, Some(f)) => {
            std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?
        }
        (None, None) => anyhow::bail!("give a SPARQL text or --file <path>"),
        (Some(_), Some(_)) => unreachable!("clap conflicts_with"),
    };
    let report = rsntr::rdfcmd::sparql_report(
        &args.dir,
        prefer,
        args.peer.as_deref(),
        &text,
        args.offline,
        args.timeout_ms,
    )
    .await?;
    Ok(print_report(&report, json))
}

async fn cmd_turtle(command: TurtleCommand, json: bool, prefer: rsntr::Prefer) -> Result<i32> {
    match command {
        TurtleCommand::Load {
            file,
            peer,
            chunk_bytes,
            dir,
            offline,
        } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            match rsntr::rdfcmd::turtle_load(
                &dir,
                prefer,
                peer.as_deref(),
                &text,
                offline,
                chunk_bytes,
            )
            .await?
            {
                rsntr::rdfcmd::LoadOutcome::Loaded { triples, chunks } => {
                    if json {
                        println!(
                            "{}",
                            json!({ "ok": true, "triples": triples, "chunks": chunks })
                        );
                    } else {
                        println!(
                            "loaded {triples} triple(s) in {chunks} chunk(s){}",
                            peer.map(|p| format!(" into {p}")).unwrap_or_default()
                        );
                    }
                    Ok(EXIT_OK)
                }
                rsntr::rdfcmd::LoadOutcome::Refused(report) => Ok(print_report(&report, json)),
            }
        }
        TurtleCommand::Dump { peer, dir, offline } => {
            let report = rsntr::rdfcmd::turtle_dump(&dir, prefer, peer.as_deref(), offline).await?;
            Ok(print_report(&report, json))
        }
    }
}

#[cfg(unix)]
async fn cmd_pipe(command: PipeCommand, json: bool, prefer: rsntr::Prefer) -> Result<i32> {
    use rsntr::pipecmd;
    match command {
        PipeCommand::Add {
            name,
            command,
            one_way,
            note,
            dir,
        } => {
            pipecmd::pipe_add(&dir, prefer, &name, &command, one_way, note.as_deref()).await?;
            if json {
                println!(
                    "{}",
                    json!({ "ok": true, "name": name, "one_way": one_way })
                );
            } else {
                println!("pipe {name} registered");
            }
            Ok(EXIT_OK)
        }
        PipeCommand::List { dir } => {
            let rows = pipecmd::pipe_list(&dir, prefer).await?;
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "pipes": rows.iter().map(|p| json!({
                            "name": p.name,
                            "command": p.command,
                            "duplex": p.duplex,
                            "note": p.note,
                        })).collect::<Vec<_>>(),
                    })
                );
            } else {
                for p in rows {
                    println!(
                        "{}\t{}\t{}",
                        p.name,
                        if p.duplex { "duplex" } else { "one-way" },
                        p.command,
                    );
                }
            }
            Ok(EXIT_OK)
        }
        PipeCommand::Rm { name, dir } => {
            if !pipecmd::pipe_rm(&dir, prefer, &name).await? {
                anyhow::bail!("no pipe named {name:?}");
            }
            if json {
                println!("{}", json!({ "ok": true, "name": name, "removed": true }));
            } else {
                println!("pipe {name} removed");
            }
            Ok(EXIT_OK)
        }
        PipeCommand::Open {
            peer,
            name,
            dir,
            offline,
        } => match client::run_duplex(&dir, &peer, &name, offline, None).await? {
            client::TalkOutcome::Done => {
                if json {
                    eprintln!("{}", json!({ "ok": true }));
                }
                Ok(EXIT_OK)
            }
            client::TalkOutcome::Denied(d) => {
                let reason = d.reason.unwrap_or_default();
                if json {
                    eprintln!(
                        "{}",
                        json!({ "ok": false, "error": { "code": "denied", "reason": reason } })
                    );
                } else {
                    eprintln!("denied: {reason}");
                }
                Ok(EXIT_DENIED)
            }
            client::TalkOutcome::Failed(e) => {
                let reason = e.reason.unwrap_or_default();
                if json {
                    eprintln!(
                        "{}",
                        json!({ "ok": false, "error": { "code": e.code, "reason": reason } })
                    );
                } else {
                    eprintln!("error [{}] {reason}", e.code);
                }
                Ok(EXIT_ERROR)
            }
        },
        PipeCommand::Accept { name, dir } => {
            pipecmd::pipe_accept(&dir, prefer, &name).await?;
            Ok(EXIT_OK)
        }
    }
}

/// Prints one csv-command pipeline failure and returns its exit code.
fn print_csv_failure(f: &rsntr::csvcmd::Failure, json: bool) -> i32 {
    use resonator_web::ApiFailure;
    match f {
        ApiFailure::Denied { reason } => {
            if json {
                println!("{}", json!({ "ok": false, "denied": reason }));
            } else {
                eprintln!(
                    "denied: {}",
                    reason.as_deref().unwrap_or("(no reason given)")
                );
            }
            EXIT_DENIED
        }
        ApiFailure::Error { code, reason } => {
            if json {
                println!(
                    "{}",
                    json!({ "ok": false, "error": { "code": code, "reason": reason } })
                );
            } else {
                eprintln!("error [{code}]: {reason}");
            }
            output::error_exit_code(code)
        }
    }
}

fn cmd_sql(
    dir: PathBuf,
    stmt: Option<String>,
    file: Option<PathBuf>,
    json: bool,
    prefer: rsntr::Prefer,
) -> Result<i32> {
    let source = match (stmt, file) {
        (Some(s), None) => s,
        (None, Some(f)) => {
            std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?
        }
        (None, None) => anyhow::bail!("give one SQL statement or --file <path>"),
        (Some(_), Some(_)) => unreachable!("clap conflicts_with"),
    };
    let outcome = rsntr::sqlcmd::run_sql(&dir, &source, prefer)?;
    if json {
        let mut out = json!({
            "ok": true,
            "statements": outcome.statements,
            "affected": outcome.affected,
        });
        if let Some((columns, rows, done)) = &outcome.rows {
            out["columns"] = json!(columns);
            out["rows"] = json!(
                rows.iter()
                    .map(|r| output::row_to_json(columns, r))
                    .collect::<Vec<_>>()
            );
            out["row_count"] = json!(done.row_count);
            out["truncated"] = json!(done.truncated);
        }
        println!("{out}");
    } else if let Some((columns, rows, done)) = &outcome.rows {
        if !columns.is_empty() {
            println!("{}", columns.join("\t"));
            for row in rows {
                println!("{}", output::render_row(columns, row));
            }
        }
        eprintln!(
            "ok: {} row(s){}",
            done.row_count.unwrap_or(rows.len() as i64),
            if done.truncated { " (truncated)" } else { "" }
        );
    } else {
        println!(
            "applied {} statement{} ({} row{} affected)",
            outcome.statements,
            if outcome.statements == 1 { "" } else { "s" },
            outcome.affected,
            if outcome.affected == 1 { "" } else { "s" },
        );
    }
    Ok(EXIT_OK)
}

async fn cmd_csv(command: CsvCommand, json: bool, prefer: rsntr::Prefer) -> Result<i32> {
    match command {
        CsvCommand::Export { dir, table, file } => {
            let doc = match rsntr::csvcmd::csv_export_with(&dir, prefer, &table).await? {
                Ok(doc) => doc,
                Err(f) => return Ok(print_csv_failure(&f, json)),
            };
            match file {
                Some(path) => {
                    std::fs::write(&path, &doc)
                        .with_context(|| format!("writing {}", path.display()))?;
                    if json {
                        println!(
                            "{}",
                            json!({
                                "ok": true,
                                "table": table,
                                "file": path.display().to_string(),
                                "bytes": doc.len(),
                            })
                        );
                    } else {
                        println!(
                            "exported {table} to {} ({} bytes)",
                            path.display(),
                            doc.len()
                        );
                    }
                }
                None => {
                    if json {
                        println!("{}", json!({ "ok": true, "table": table, "csv": doc }));
                    } else {
                        print!("{doc}");
                    }
                }
            }
            Ok(EXIT_OK)
        }
        CsvCommand::Import {
            dir,
            table,
            file,
            create,
        } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            match rsntr::csvcmd::csv_import_with(&dir, prefer, &table, &text, create).await? {
                Ok(report) => {
                    if json {
                        println!(
                            "{}",
                            json!({
                                "ok": true,
                                "table": report.table,
                                "created": report.created,
                                "rows_inserted": report.rows_inserted,
                            })
                        );
                    } else {
                        println!(
                            "imported {} row(s) into {}{}",
                            report.rows_inserted,
                            report.table,
                            if report.created { " (created)" } else { "" }
                        );
                    }
                    Ok(EXIT_OK)
                }
                Err(f) => Ok(print_csv_failure(&f, json)),
            }
        }
    }
}

async fn cmd_inbox(command: InboxCommand, json: bool, prefer: rsntr::Prefer) -> Result<i32> {
    match command {
        InboxCommand::List { all, dir } => {
            let rows = rsntr::inboxcmd::inbox_list_with(&dir, prefer, all).await?;
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "inbox": rows
                            .iter()
                            .map(|r| json!({
                                "request_id": r.request_id,
                                "peer": r.peer,
                                "kind": r.kind,
                                "summary": r.summary,
                                "decision": r.decision,
                                "received_at": r.received_at,
                            }))
                            .collect::<Vec<_>>(),
                    })
                );
            } else if rows.is_empty() {
                eprintln!("inbox is empty");
            } else {
                for r in rows {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        r.request_id,
                        r.received_at,
                        r.peer,
                        r.kind,
                        r.decision.as_deref().unwrap_or("pending"),
                        r.summary,
                    );
                }
            }
            Ok(EXIT_OK)
        }
        InboxCommand::Answer {
            id,
            verdict,
            remember,
            grants,
            dir,
        } => {
            let report = rsntr::inboxcmd::inbox_answer_with(
                &dir,
                prefer,
                &id,
                verdict == "allow",
                remember,
                &grants,
            )
            .await?;
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "request_id": report.request_id,
                        "peer": report.peer,
                        "decision": report.decision,
                        "knock": report.knock,
                        "remembered": report.remembered,
                        "granted": report.granted,
                    })
                );
            } else {
                let what = if report.knock {
                    if report.decision == "allow" {
                        if report.granted.is_empty() {
                            format!(
                                "knock answered: {} admitted\n\
                                 note: admission alone grants nothing - every request still needs \
                                 _policy rows (re-answer is not possible; add rows with \
                                 --grant next time, or insert into _policy directly)",
                                report.peer
                            )
                        } else {
                            format!(
                                "knock answered: {} admitted; granted {}",
                                report.peer,
                                report.granted.join(", ")
                            )
                        }
                    } else {
                        format!("knock answered: {} denied", report.peer)
                    }
                } else if report.remembered.is_empty() {
                    format!(
                        "{} for {} (cached for identical requests)",
                        report.decision, report.peer
                    )
                } else {
                    format!(
                        "{} for {} (cached; policy rows written for {})",
                        report.decision,
                        report.peer,
                        report.remembered.join(", ")
                    )
                };
                println!("{what}");
            }
            Ok(EXIT_OK)
        }
    }
}

/// Prints one query/help report and returns its exit code.
fn print_report(report: &client::QueryReport, json: bool) -> i32 {
    if json {
        println!("{}", output::report_to_json(report));
        return output::report_exit_code(report);
    }
    match &report.outcome {
        client::QueryOutcome::Rows {
            columns,
            rows,
            done,
        } => {
            if !columns.is_empty() {
                println!("{}", columns.join("\t"));
                for row in rows {
                    println!("{}", output::render_row(columns, row));
                }
            }
            if let Some(affected) = done.affected_rows {
                eprintln!("ok: {affected} row(s) affected");
            } else {
                eprintln!(
                    "ok: {} row(s){}",
                    done.row_count.unwrap_or(rows.len() as i64),
                    if done.truncated { " (truncated)" } else { "" }
                );
            }
            EXIT_OK
        }
        client::QueryOutcome::Graph { triples, done } => {
            for t in triples {
                println!("{}", output::triple_string(t));
            }
            eprintln!(
                "ok: {} triple(s){}",
                done.row_count.unwrap_or(triples.len() as i64),
                if done.truncated { " (truncated)" } else { "" }
            );
            EXIT_OK
        }
        client::QueryOutcome::Help { text, topics } => {
            println!("{text}");
            if !topics.is_empty() {
                eprintln!("topics: {}", topics.join(", "));
            }
            EXIT_OK
        }
        client::QueryOutcome::Denied(d) => {
            eprintln!(
                "denied: {}",
                d.reason.as_deref().unwrap_or("(no reason given)")
            );
            EXIT_DENIED
        }
        client::QueryOutcome::Failed(e) => {
            eprintln!(
                "error [{}]: {}",
                e.code,
                e.reason.as_deref().unwrap_or("(no reason given)")
            );
            output::error_exit_code(&e.code)
        }
    }
}

async fn cmd_knock(args: KnockArgs, json: bool) -> Result<i32> {
    let report = client::knock(&args.dir, &args.peer, &args.message, args.offline).await?;
    match &report.decision {
        Some(d) => {
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": d.decision != "deny",
                        "id": report.id,
                        "decision": d.decision,
                        "decided_by": d.decided_by,
                        "reason": d.reason,
                    })
                );
            } else {
                let why = d
                    .reason
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default();
                println!("knock {}: {}{}", report.id, d.decision, why);
            }
            Ok(if d.decision == "deny" {
                EXIT_DENIED
            } else {
                EXIT_OK
            })
        }
        None => {
            if json {
                println!(
                    "{}",
                    json!({ "ok": false, "id": report.id, "decision": "silence" })
                );
            } else {
                println!(
                    "knock {}: no answer (knocks over budget are dropped silently; try later)",
                    report.id
                );
            }
            Ok(EXIT_ERROR)
        }
    }
}

async fn cmd_query(args: QueryArgs, json: bool) -> Result<i32> {
    let report = client::run_query(
        &args.dir,
        &args.peer,
        &args.modulation,
        &args.signal,
        &args.params,
        args.offline,
        args.timeout_ms,
    )
    .await?;
    Ok(print_report(&report, json))
}

async fn cmd_help(args: HelpArgs, json: bool) -> Result<i32> {
    let report = client::run_help(&args.dir, &args.peer, args.topic, args.offline).await?;
    Ok(print_report(&report, json))
}

async fn cmd_projection(args: ProjectionArgs, json: bool) -> Result<i32> {
    use std::io::{BufRead, IsTerminal, Write};

    let projection = match fetch_projection(&args, &args.path, json).await? {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };

    let Some(n) = args.choose else {
        if json {
            println!("{}", output::projection_to_json(&args.path, &projection));
        } else {
            print!(
                "{}",
                teletype::render_projection(&args.peer, &args.path, &projection)
            );
        }
        return Ok(EXIT_OK);
    };
    if !json {
        print!(
            "{}",
            teletype::render_projection(&args.peer, &args.path, &projection)
        );
    }
    let point = n
        .checked_sub(1)
        .and_then(|i| projection.offers.get(i))
        .with_context(|| format!("no menu entry [{n}]"))?;

    // Field values: --field name=value flags first, then a terminal
    // prompt for whatever the coupling still needs (never in --json
    // mode: agents must pass every field).
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    for pair in &args.fields {
        let (name, value) = pair
            .split_once('=')
            .with_context(|| format!("--field expects name=value, got {pair:?}"))?;
        values.insert(name.to_string(), value.to_string());
    }
    if !json && std::io::stdin().is_terminal() {
        let stdin = std::io::stdin();
        for f in &point.coupling {
            if values.contains_key(&f.name) {
                continue;
            }
            let mut prompt = f.name.clone();
            if let Some(h) = &f.hint {
                prompt.push_str(&format!(" ({h})"));
            }
            if !f.one_of.is_empty() {
                let choices: Vec<String> = f.one_of.iter().map(teletype::render_choice).collect();
                prompt.push_str(&format!(" [{}]", choices.join("/")));
            }
            if !f.required {
                prompt.push('?');
            }
            eprint!("{prompt}: ");
            std::io::stderr().flush().ok();
            let mut line = String::new();
            stdin.lock().read_line(&mut line)?;
            let line = line.trim();
            if !line.is_empty() {
                values.insert(f.name.clone(), line.to_string());
            }
        }
    }

    match teletype::build_invocation(point, &values)? {
        teletype::Invocation::Zoom { path } => match fetch_projection(&args, &path, json).await? {
            Ok(deeper) => {
                if json {
                    println!("{}", output::projection_to_json(&path, &deeper));
                } else {
                    print!(
                        "{}",
                        teletype::render_projection(&args.peer, &path, &deeper)
                    );
                }
                Ok(EXIT_OK)
            }
            Err(code) => Ok(code),
        },
        teletype::Invocation::Entrainable => {
            if json {
                println!("{}", json!({ "ok": true, "entrainable": point.iri }));
            } else {
                println!(
                    "a signal; entrain it:\n  rsntr entrain {} {}",
                    args.peer, point.iri
                );
            }
            Ok(EXIT_OK)
        }
        teletype::Invocation::Statement {
            kind,
            modulation,
            text,
            params,
        } => {
            let report = client::run_statement(
                &args.dir,
                &args.peer,
                kind,
                &modulation,
                &text,
                params,
                args.offline,
                None,
            )
            .await?;
            Ok(print_report(&report, json))
        }
    }
}

/// Fetches one projection; a refusal is printed and turned into an exit
/// code instead of an Err, so `--json` stays a stable single object.
async fn fetch_projection(
    args: &ProjectionArgs,
    path: &str,
    json: bool,
) -> Result<std::result::Result<resonator_protocol::Projection, i32>> {
    match client::run_projection(&args.dir, &args.peer, path, args.offline).await? {
        client::ProjectionOutcome::Projection(p) => Ok(Ok(p)),
        client::ProjectionOutcome::Denied(d) => {
            if json {
                println!("{}", json!({ "ok": false, "denied": d.reason }));
            } else {
                eprintln!(
                    "denied: {}",
                    d.reason.as_deref().unwrap_or("(no reason given)")
                );
            }
            Ok(Err(EXIT_DENIED))
        }
        client::ProjectionOutcome::Failed(e) => {
            if json {
                println!(
                    "{}",
                    json!({ "ok": false, "error": { "code": e.code, "reason": e.reason } })
                );
            } else {
                eprintln!(
                    "error [{}]: {}",
                    e.code,
                    e.reason.as_deref().unwrap_or("(no reason given)")
                );
            }
            Ok(Err(output::error_exit_code(&e.code)))
        }
    }
}

async fn cmd_entrain(args: EntrainArgs, json: bool) -> Result<i32> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let (damp_tx, damp_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(client::run_entrain(
        args.dir.clone(),
        args.peer.clone(),
        args.point.clone(),
        args.offline,
        tx,
        damp_rx,
    ));
    let mut damp_tx = Some(damp_tx);
    let mut code = EXIT_OK;
    loop {
        tokio::select! {
            item = rx.recv() => match item {
                None => break,
                Some(client::EntrainItem::Entrained) => {
                    if json {
                        println!("{}", json!({ "entrained": args.point }));
                    } else {
                        eprintln!("entrained to <{}>; ctrl-c to damp", args.point);
                    }
                }
                Some(client::EntrainItem::Vibration(v)) => {
                    if json {
                        println!(
                            "{}",
                            json!({
                                "vibration": {
                                    "seq": v.seq,
                                    "point": v.point,
                                    "at": v.at,
                                    "payload": v.payload
                                        .iter()
                                        .map(output::triple_string)
                                        .collect::<Vec<_>>(),
                                }
                            })
                        );
                    } else {
                        let at = v.at.map(|t| format!(" at {t}")).unwrap_or_default();
                        println!("vibration #{} from <{}>{at}", v.seq, v.point);
                    }
                }
                Some(client::EntrainItem::Damped) => {
                    if json {
                        println!("{}", json!({ "damped": true }));
                    } else {
                        eprintln!("damped");
                    }
                    break;
                }
                Some(client::EntrainItem::Denied(d)) => {
                    if json {
                        println!("{}", json!({ "ok": false, "denied": d.reason }));
                    } else {
                        eprintln!(
                            "denied: {}",
                            d.reason.as_deref().unwrap_or("(no reason given)")
                        );
                    }
                    code = EXIT_DENIED;
                    break;
                }
                Some(client::EntrainItem::Failed(e)) => {
                    if json {
                        println!(
                            "{}",
                            json!({ "ok": false, "error": { "code": e.code, "reason": e.reason } })
                        );
                    } else {
                        eprintln!(
                            "error [{}]: {}",
                            e.code,
                            e.reason.as_deref().unwrap_or("(no reason given)")
                        );
                    }
                    code = output::error_exit_code(&e.code);
                    break;
                }
            },
            _ = tokio::signal::ctrl_c(), if damp_tx.is_some() => {
                let _ = damp_tx.take().expect("armed").send(());
            }
        }
    }
    task.await.context("joining the entrain task")??;
    Ok(code)
}

async fn cmd_chat_send(
    dir: PathBuf,
    target: String,
    text: String,
    file: Option<PathBuf>,
    json: bool,
    prefer: rsntr::Prefer,
) -> Result<i32> {
    let report = rsntr::chat::chat_send_with(&dir, prefer, &target, &text, file.as_deref()).await?;
    if json {
        println!(
            "{}",
            json!({
                "ok": true,
                "message_id": report.message_id,
                "scope": report.scope,
                "queued_to": report.queued_to,
                "blob": report.blob.as_ref().map(|(hash, bytes)| json!({
                    "hash": hash,
                    "bytes": bytes,
                })),
                "spilled": report.spilled,
            })
        );
    } else {
        println!("{}", report.message_id);
        if report.spilled {
            eprintln!("body spilled to a text attachment (readers inline it back)");
        } else if let Some((hash, bytes)) = &report.blob {
            eprintln!("attached {hash} ({bytes} bytes)");
        }
        eprintln!("queued to {}", report.queued_to.join(", "));
    }
    Ok(EXIT_OK)
}

/// One stderr hint per failed spill inline, naming the remedy (the
/// sender's dial hints are usually the culprit).
fn report_inline_failures(entries: &[rsntr::chat::LogEntry], failed: &[String]) {
    for id in failed {
        let sender = entries
            .iter()
            .find(|e| &e.id == id)
            .map(|e| e.sender.clone())
            .unwrap_or_default();
        eprintln!(
            "warning: could not fetch the full body of {id} from {sender}; \
             showing the preview (refresh its dial hints with \
             `rsntr peer add <name> <its current ticket>` and re-read)"
        );
    }
}

fn chat_entry_json(entry: &rsntr::chat::LogEntry) -> serde_json::Value {
    json!({
        "id": entry.id,
        "scope": entry.scope,
        "sender": entry.sender,
        "at": entry.at,
        "received_at": entry.received_at,
        "body": entry.body,
        "blob_hash": entry.blob_hash,
        "blob_bytes": entry.blob_bytes,
        "blob_name": entry.blob_name,
        "outgoing": entry.outgoing,
        "status": entry.status,
    })
}

fn chat_entry_line(entry: &rsntr::chat::LogEntry) -> String {
    let who = if entry.outgoing {
        "me".to_string()
    } else {
        let s = &entry.sender;
        if s.len() > 8 {
            s[..8].to_string()
        } else {
            s.clone()
        }
    };
    let status = entry
        .status
        .as_deref()
        .map(|s| format!(" [{s}]"))
        .unwrap_or_default();
    let blob = entry
        .blob_name
        .as_deref()
        .map(|n| format!(" (file: {n})"))
        .unwrap_or_default();
    format!("{} {who}{status}: {}{blob}", entry.at, entry.body)
}

async fn cmd_chat_log(
    dir: &std::path::Path,
    target: &str,
    limit: i64,
    since: Option<&str>,
    no_inline: bool,
    json: bool,
) -> Result<i32> {
    let mut entries = rsntr::chat::chat_log(dir, target, limit, since)?;
    let inline_failed = if no_inline {
        Vec::new()
    } else {
        rsntr::chat::inline_spilled(dir, &mut entries).await
    };
    report_inline_failures(&entries, &inline_failed);
    if json {
        println!(
            "{}",
            json!({
                "ok": true,
                "messages": entries.iter().map(chat_entry_json).collect::<Vec<_>>(),
                "inline_failed": inline_failed,
            })
        );
    } else {
        // Stored newest first; print oldest first, chat style.
        for entry in entries.iter().rev() {
            println!("{}", chat_entry_line(entry));
        }
    }
    Ok(EXIT_OK)
}

async fn cmd_chat_wait(
    dir: PathBuf,
    target: Option<String>,
    timeout: u64,
    no_inline: bool,
    json: bool,
) -> Result<i32> {
    let mut report = rsntr::chat::chat_wait(
        &dir,
        target.as_deref(),
        std::time::Duration::from_secs(timeout),
    )
    .await?;
    let inline_failed = if no_inline {
        Vec::new()
    } else {
        rsntr::chat::inline_spilled(&dir, &mut report.messages).await
    };
    report_inline_failures(&report.messages, &inline_failed);
    if json {
        println!(
            "{}",
            json!({
                "ok": true,
                "timed_out": report.timed_out,
                "messages": report.messages.iter().map(chat_entry_json).collect::<Vec<_>>(),
                "next_since": report.next_since,
                "inline_failed": inline_failed,
            })
        );
    } else if report.timed_out {
        eprintln!("nothing arrived within {timeout}s");
    } else {
        for entry in &report.messages {
            println!("{}", chat_entry_line(entry));
        }
    }
    Ok(if report.timed_out {
        rsntr::EXIT_TIMEOUT
    } else {
        EXIT_OK
    })
}

async fn cmd_chat_watch(dir: PathBuf, target: String, json: bool) -> Result<i32> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let (damp_tx, damp_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(rsntr::chat::chat_watch(dir, target.clone(), tx, damp_rx));
    let mut damp_tx = Some(damp_tx);
    let mut code = EXIT_OK;
    loop {
        tokio::select! {
            item = rx.recv() => match item {
                None => break,
                Some(rsntr::chat::WatchEvent::Entrained) => {
                    if json {
                        println!("{}", json!({ "watching": target }));
                    } else {
                        eprintln!("watching {target}; ctrl-c to stop");
                    }
                }
                Some(rsntr::chat::WatchEvent::Message(entry)) => {
                    if json {
                        println!("{}", chat_entry_json(&entry));
                    } else {
                        println!("{}", chat_entry_line(&entry));
                    }
                }
                Some(rsntr::chat::WatchEvent::Damped) => {
                    if json {
                        println!("{}", json!({ "damped": true }));
                    } else {
                        eprintln!("stopped");
                    }
                    break;
                }
                Some(rsntr::chat::WatchEvent::Denied(reason)) => {
                    if json {
                        println!("{}", json!({ "ok": false, "denied": reason }));
                    } else {
                        eprintln!("denied: {reason}");
                    }
                    code = EXIT_DENIED;
                    break;
                }
                Some(rsntr::chat::WatchEvent::Failed(message)) => {
                    if json {
                        println!(
                            "{}",
                            json!({ "ok": false, "error": { "code": "watch", "reason": message } })
                        );
                    } else {
                        eprintln!("error: {message}");
                    }
                    code = EXIT_ERROR;
                    break;
                }
            },
            _ = tokio::signal::ctrl_c(), if damp_tx.is_some() => {
                let _ = damp_tx.take().expect("armed").send(());
            }
        }
    }
    task.await.context("joining the watch task")??;
    Ok(code)
}

fn cmd_chat_room(command: RoomCommand, json: bool, prefer: rsntr::Prefer) -> Result<i32> {
    match command {
        RoomCommand::Create { name, dir } => {
            let room_iri = rsntr::chat::room_create_with(&dir, prefer, &name)?;
            if json {
                println!("{}", json!({ "ok": true, "room": room_iri, "name": name }));
            } else {
                println!("{room_iri}");
            }
            Ok(EXIT_OK)
        }
        RoomCommand::Add { room, peer, dir } => {
            let (room_iri, peer_hex) = rsntr::chat::room_add_with(&dir, prefer, &room, &peer)?;
            if json {
                println!(
                    "{}",
                    json!({ "ok": true, "room": room_iri, "member": peer_hex })
                );
            } else {
                println!("added {peer_hex} to {room_iri}");
            }
            Ok(EXIT_OK)
        }
        RoomCommand::Join {
            peer,
            room_iri,
            name,
            dir,
        } => {
            let host =
                rsntr::chat::room_join_with(&dir, prefer, &peer, &room_iri, name.as_deref())?;
            if json {
                println!("{}", json!({ "ok": true, "room": room_iri, "host": host }));
            } else {
                println!("joined {room_iri} (host {host})");
            }
            Ok(EXIT_OK)
        }
    }
}

async fn cmd_fetch(args: FetchArgs, json: bool) -> Result<i32> {
    use std::io::Write;
    let outcome = client::run_fetch(
        &args.dir,
        &args.peer,
        &args.hash,
        args.output.as_deref(),
        args.offline,
        std::time::Duration::from_secs(30),
    )
    .await?;
    match outcome {
        client::FetchOutcome::Written { path, bytes } => {
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "path": path.display().to_string(),
                        "bytes": bytes,
                    })
                );
            } else {
                eprintln!("fetched {bytes} byte(s) to {}", path.display());
            }
            Ok(EXIT_OK)
        }
        client::FetchOutcome::Bytes(bytes) => {
            let n = bytes.len();
            let mut out = std::io::stdout().lock();
            out.write_all(&bytes).context("writing the blob")?;
            out.flush().context("flushing the blob")?;
            if json {
                eprintln!("{}", json!({ "ok": true, "bytes": n }));
            } else {
                eprintln!("fetched {n} byte(s)");
            }
            Ok(EXIT_OK)
        }
        client::FetchOutcome::Unreachable(reason) => {
            if json {
                println!(
                    "{}",
                    json!({ "ok": false, "error": { "code": "timeout", "reason": reason } })
                );
            } else {
                eprintln!("provider unreachable: {reason} (retry when it is back up)");
            }
            Ok(rsntr::EXIT_TIMEOUT)
        }
    }
}

async fn cmd_watch(args: WatchArgs, json: bool) -> Result<i32> {
    use std::io::Write;

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let task = {
        let dir = args.dir.clone();
        let peer = args.peer.clone();
        let source = args.source.clone();
        tokio::spawn(async move {
            client::run_media_channel(&dir, &peer, &source, args.offline, tx).await
        })
    };

    let mut out = std::io::stdout().lock();
    let mut bytes: u64 = 0;
    let mut code = EXIT_OK;
    while let Some(chunk) = rx.recv().await {
        match chunk {
            client::MediaChunk::Header { content_type } => {
                // Header to stderr: stdout is exclusively the byte feed.
                if json {
                    eprintln!("{}", json!({ "media": { "content_type": content_type } }));
                } else {
                    eprintln!("content-type: {content_type}");
                }
            }
            client::MediaChunk::Data(chunk) => {
                out.write_all(&chunk).context("writing the media feed")?;
                bytes += chunk.len() as u64;
            }
            client::MediaChunk::Denied(d) => {
                if json {
                    eprintln!("{}", json!({ "ok": false, "denied": d.reason }));
                } else {
                    eprintln!(
                        "denied: {}",
                        d.reason.as_deref().unwrap_or("(no reason given)")
                    );
                }
                code = EXIT_DENIED;
            }
            client::MediaChunk::Failed(e) => {
                if json {
                    eprintln!(
                        "{}",
                        json!({ "ok": false, "error": { "code": e.code, "reason": e.reason } })
                    );
                } else {
                    eprintln!(
                        "error [{}]: {}",
                        e.code,
                        e.reason.as_deref().unwrap_or("(no reason given)")
                    );
                }
                code = output::error_exit_code(&e.code);
            }
        }
    }
    out.flush().context("flushing the media feed")?;
    task.await.context("joining the watch task")??;
    if code == EXIT_OK {
        if json {
            eprintln!("{}", json!({ "ok": true, "bytes": bytes }));
        } else {
            eprintln!("feed ended: {bytes} byte(s)");
        }
    }
    Ok(code)
}
