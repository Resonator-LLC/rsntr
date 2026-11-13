//! The `resonator` binary: the command line over `resonator-node`, which
//! assembles the wave server with the robots, tools and agents of
//! `resonator-agents`, federation and the embedded web client.
//!
//! `serve` starts the node (`resonator_node::launch`) and on Ctrl-C or
//! SIGTERM stops it: federation, then the agents (cancelling responses
//! being written), then the server. With `mode = "device"` the sign-in
//! URL (with the loopback token) is written to `device.url` in the data
//! directory.
//!
//! With `[federation] enabled = true`, `serve` loads (or creates) the
//! server key before the wave server starts, so that every delta it
//! applies is signed, and starts the federation endpoint once the server
//! runs; it is stopped first on shutdown. `federation key` prints the key
//! and the DNS record that binds the domain to it.
//!
//! With `[person] enabled = true` as well, the node is one device of a
//! person: its domain is the person domain (set from the person key or the
//! device certificate before the wave server starts). The `person`
//! commands manage the person key (`init`, `recover`, `show`), certify
//! devices (`cert`) and revoke them (`revoke`).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use resonator_federation::{FederationSetup, ServerKey};
use resonator_server::config::{Config, Overrides};

#[derive(Parser)]
#[command(name = "resonator", version, about = "A single-binary wave server")]
struct Cli {
    /// Path of the TOML config file [default: resonator.toml if present].
    #[arg(
        long,
        short,
        global = true,
        env = "RESONATOR_CONFIG",
        value_name = "FILE"
    )]
    config: Option<PathBuf>,
    #[command(flatten)]
    overrides: Overrides,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server until Ctrl-C.
    Serve,
    /// Write a consistent copy of the database to PATH; safe while the
    /// server is running.
    Backup {
        /// Where to write the copy; must not exist.
        path: PathBuf,
    },
    /// Print the effective configuration as TOML.
    Config,
    /// Server-to-server federation.
    Federation {
        #[command(subcommand)]
        command: FederationCommand,
    },
    /// Person identity: a person key whose devices share one domain.
    Person {
        #[command(subcommand)]
        command: PersonCommand,
    },
}

#[derive(Subcommand)]
enum PersonCommand {
    /// Create a person key and store it in person.key_path. Prints its
    /// 24-word recovery phrase once: write it down, it is the only way to
    /// recover the person key.
    Init,
    /// Recover the person on this (fresh) device from its recovery
    /// phrase, read from standard input: stores the person key (an earlier
    /// key and device certificate are moved aside); at its next start the
    /// node issues itself a certificate, publishes a new revocation list,
    /// pulls the person's waves back from other participants' servers and
    /// takes the host lease. Needs `[person] enabled = true, admin = true`.
    Recover {
        /// Revoke every other device of the person (a lost or stolen
        /// device).
        #[arg(long)]
        revoke_old: bool,
        /// A server or device to ask for the person's waves:
        /// `<key>[@host:port]`; repeatable. Configured peers and any
        /// server met are asked too.
        #[arg(long = "peer", value_name = "PEER")]
        peers: Vec<String>,
    },
    /// Pair a new device by link.
    Pair {
        #[command(subcommand)]
        command: PairCommand,
    },
    /// Let another server stand in as host of this person's waves while
    /// no device of the person is around.
    Cohost {
        #[command(subcommand)]
        command: CohostCommand,
    },
    /// Certify a device key (52 characters, from `resonator federation
    /// key` on the device) as a device of this person; prints the
    /// certificate to put into the device's person.cert or device.cert.
    Cert {
        /// The device's key.
        device: String,
        /// The device holds the person key too.
        #[arg(long)]
        admin: bool,
        /// A name for the device.
        #[arg(long, default_value = "")]
        name: String,
    },
    /// Revoke a device key: its connections are cut and its new deltas
    /// refused (those applied before stay). A running server picks the
    /// revocation up from the database and tells its peers.
    Revoke {
        /// The device's key.
        device: String,
    },
    /// Show the person domain, this device and the devices known.
    Show {
        /// Also print the recovery phrase (admin devices).
        #[arg(long)]
        phrase: bool,
    },
}

#[derive(Subcommand)]
enum PairCommand {
    /// Make a one-time pairing invitation (valid for 10 minutes) on this
    /// admin device, which its running node answers. Prints a short code
    /// (when the pairing rendezvous can be reached) and a link: show them
    /// only to yourself, they let a device in.
    Invite {
        /// The new device gets the person key too (asks for confirmation
        /// here: `person pair confirm`).
        #[arg(long)]
        admin: bool,
    },
    /// List the open invitations.
    List,
    /// Confirm the admin pairing a device asked for.
    Confirm {
        /// The invitation id, from `person pair list`.
        id: String,
    },
    /// Withdraw an invitation (and its code).
    Cancel { id: String },
    /// Pair this device by a pairing code (`482913-ocean-tiger`; spaces
    /// work too) or a `resonator:pair/...` link; it is a device of that
    /// person from its next start. Stop the node here first.
    Join {
        #[arg(required = true, num_args = 1.., value_name = "CODE-OR-LINK")]
        invitation: Vec<String>,
        /// A name for this device.
        #[arg(long, default_value = "")]
        name: String,
        /// Seconds to wait for the inviting device to confirm an admin
        /// pairing.
        #[arg(long, default_value_t = 180)]
        wait: u64,
    },
}

#[derive(Subcommand)]
enum CohostCommand {
    /// Grant a server's domain co-hosting: once the host lease has run out
    /// and no device of the person is around, its server may host the
    /// person's waves until a device is back.
    Grant {
        domain: String,
        /// For how many days.
        #[arg(long, default_value_t = 30)]
        days: u64,
    },
    /// Withdraw a co-host grant.
    Revoke { domain: String },
}

#[derive(Subcommand)]
enum FederationCommand {
    /// Print the server key (creating it if there is none) and the DNS TXT
    /// record that binds the domain to it.
    Key,
    /// List the keys seen for other domains and their status (trusted,
    /// retired, quarantined, denied), or approve or deny one. Works on the
    /// database, also while the server runs; a decision applies to the
    /// next connection with that server.
    Peers {
        #[command(subcommand)]
        action: Option<PeersAction>,
    },
}

#[derive(Subcommand)]
enum PeersAction {
    /// Trust a key: a 52-character key, or a domain whose one quarantined
    /// key to trust.
    Approve {
        /// A key or a domain.
        peer: String,
    },
    /// Refuse a key: a 52-character key, or a domain whose trusted and
    /// quarantined keys to refuse.
    Deny {
        /// A key or a domain.
        peer: String,
    },
}

/// Resolves on Ctrl-C or SIGTERM.
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn serve(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let mut node = resonator_node::launch(config).await?;
    loop {
        if node.token().is_some() {
            // The URL carries the token: only its file is named.
            tracing::info!(
                path = %node.app().device_url_path().display(),
                "device mode: open the URL in this file to sign in"
            );
        }
        tokio::select! {
            () = stop_signal() => break,
            // Paired or recovered from the settings: start again as the
            // device it is now.
            () = node.restart_requested() => node = node.relaunch().await?,
        }
    }
    node.shutdown().await;
    Ok(())
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load(cli.config.as_deref(), &cli.overrides)?;
    match cli.command {
        Command::Serve => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(serve(config))?;
        }
        Command::Backup { path } => {
            resonator_node::backup(&config, &path)?;
            println!("{}", path.display());
        }
        Command::Config => print!("{}", toml::to_string(&config)?),
        Command::Federation {
            command: FederationCommand::Key,
        } => print!("{}", federation_key(&config)?),
        Command::Federation {
            command: FederationCommand::Peers { action },
        } => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            print!("{}", runtime.block_on(federation_peers(&config, action))?);
        }
        Command::Person { command } => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            print!("{}", runtime.block_on(person(&config, command))?);
        }
    }
    Ok(())
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

fn device_key(text: &str) -> Result<resonator_federation::PublicKey, Box<dyn std::error::Error>> {
    resonator_federation::parse_key_domain(text.trim())
        .ok()
        .flatten()
        .ok_or_else(|| format!("'{text}' is not a device key (52 characters)").into())
}

/// The person key of an admin device.
fn person_key(
    config: &Config,
) -> Result<resonator_federation::PersonKey, Box<dyn std::error::Error>> {
    use resonator_federation::{FileKeyStore, KeyStore, PersonKey};
    let store = FileKeyStore::new(config.person_key_path());
    match store.load()? {
        Some(phrase) => Ok(PersonKey::from_phrase(&phrase)),
        None => Err(format!(
            "{} holds no person key; run `resonator person init` or `person recover`",
            store.path().display()
        )
        .into()),
    }
}

/// Opens the database, which must exist unless `create`.
async fn open_database(
    config: &Config,
    create: bool,
) -> Result<resonator_server::storage::Database, Box<dyn std::error::Error>> {
    let path = config.database_path.clone();
    if !create && !path.exists() {
        return Err(format!("database {} does not exist", path.display()).into());
    }
    let options = config.database_options();
    Ok(tokio::task::spawn_blocking(move || {
        resonator_server::storage::Database::open(path, options)
    })
    .await??)
}

/// The output of the `person` commands. Only `init` and `show --phrase`
/// print the recovery phrase.
async fn person(
    config: &Config,
    command: PersonCommand,
) -> Result<String, Box<dyn std::error::Error>> {
    use resonator_federation::person::{
        PersonKey, RecoveryPhrase, cert_to_hex, save_cert, save_revocations, stored_certs,
        stored_revocations, verify_cert, verify_revocations,
    };
    use resonator_federation::{FileKeyStore, KeyStore};
    let store = FileKeyStore::new(config.person_key_path());
    match command {
        PersonCommand::Init => {
            if store.load()?.is_some() {
                return Err(format!(
                    "{} already holds a person key; `person show` shows it",
                    store.path().display()
                )
                .into());
            }
            let (key, phrase) = PersonKey::generate();
            store.store(&phrase)?;
            Ok(format!(
                "person domain: {}\nkey file: {}\n\nrecovery phrase (write it down now; it is \
                 not shown again and is the only way to recover the person key):\n\n  {}\n\n\
                 set [person] enabled = true, admin = true to run this node as a device of the \
                 person\n",
                key.domain(),
                store.path().display(),
                phrase.words()
            ))
        }
        PersonCommand::Recover { revoke_old, peers } => {
            eprintln!("enter the 24-word recovery phrase:");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            let phrase = RecoveryPhrase::parse(&line)?;
            line.clear();
            let key = resonator_federation::control::prepare_recovery(
                config, &phrase, revoke_old, peers,
            )?;
            let mut out = format!(
                "person domain: {}\nkey file: {}\n",
                key.domain(),
                store.path().display()
            );
            if config.person.enabled && config.person.admin {
                out.push_str(
                    "start the node: it certifies itself, publishes a new revocation list{}, \
                     pulls the person's waves back and takes the host lease\n",
                );
                out = out.replace(
                    "{}",
                    if revoke_old {
                        " revoking every other device"
                    } else {
                        ""
                    },
                );
            } else {
                out.push_str(
                    "set [person] enabled = true, admin = true and start the node to finish the \
                     recovery\n",
                );
            }
            Ok(out)
        }
        PersonCommand::Pair { command } => pair(config, command).await,
        PersonCommand::Cohost { command } => {
            let key = person_key(config)?;
            let (domain, until) = match command {
                CohostCommand::Grant { domain, days } => {
                    let until = now_millis().saturating_add(
                        i64::try_from(days.saturating_mul(86_400_000)).unwrap_or(i64::MAX),
                    );
                    (domain, until)
                }
                CohostCommand::Revoke { domain } => (domain, now_millis()),
            };
            let signed = key.grant_co_host(&domain, until, now_millis());
            let db = open_database(config, true).await?;
            let saved = resonator_federation::cohost::store_co_host(
                &db.federation_store(),
                &signed,
                now_millis(),
            )
            .await;
            db.close().await;
            let grant = saved?;
            Ok(if grant.until > now_millis() {
                format!(
                    "{domain} may co-host {}'s waves until {} (ms since the epoch); a running \
                     node tells its peers within a few seconds\n",
                    key.domain(),
                    grant.until
                )
            } else {
                format!("withdrew {domain}'s co-host grant; a running node tells its peers\n")
            })
        }
        PersonCommand::Cert {
            device,
            admin,
            name,
        } => {
            let key = person_key(config)?;
            let device = device_key(&device)?;
            let signed = key.issue_cert(&device, &name, admin, now_millis());
            let cert = verify_cert(&signed)?;
            if config.database_path.exists() {
                let db = open_database(config, false).await?;
                let saved = save_cert(&db.federation_store(), &cert, now_millis()).await;
                db.close().await;
                saved?;
            }
            Ok(format!(
                "certificate of {} for {}:\n{}\nput it into the device's [person] cert, or its \
                 data directory's device.cert\n",
                device.to_z32(),
                key.domain(),
                cert_to_hex(&signed)
            ))
        }
        PersonCommand::Revoke { device } => {
            let key = person_key(config)?;
            let device = device_key(&device)?;
            let db = open_database(config, true).await?;
            let federation = db.federation_store();
            let result = async {
                let previous = stored_revocations(&federation, &key.public()).await?;
                if previous.as_ref().is_some_and(|list| list.contains(&device)) {
                    return Ok::<_, Box<dyn std::error::Error>>(format!(
                        "{} is revoked already\n",
                        device.to_z32()
                    ));
                }
                let list =
                    verify_revocations(&key.revoke(previous.as_ref(), [device], now_millis()))?;
                save_revocations(&federation, &list, now_millis()).await?;
                Ok(format!(
                    "revoked {} (revocation list {}, {} devices); a running server spreads it \
                     within a few seconds\n",
                    device.to_z32(),
                    list.seq,
                    list.revoked.len()
                ))
            }
            .await;
            db.close().await;
            result
        }
        PersonCommand::Show { phrase } => {
            let mut out = String::new();
            let device = ServerKey::load(&config.federation_key_path())?;
            if let Some(stored) = store.load()? {
                let key = PersonKey::from_phrase(&stored);
                out.push_str(&format!(
                    "person domain: {}\nperson key file: {}\n",
                    key.domain(),
                    store.path().display()
                ));
                if phrase {
                    out.push_str(&format!("recovery phrase: {}\n", stored.words()));
                }
            } else if phrase {
                return Err("this device holds no person key".into());
            }
            match FederationSetup::person_domain(config) {
                Ok(Some(domain)) => out.push_str(&format!("this device's domain: {domain}\n")),
                Ok(None) => out.push_str("person mode is off ([person] enabled = false)\n"),
                Err(error) => out.push_str(&format!("person mode cannot start: {error}\n")),
            }
            if let Some(device) = &device {
                out.push_str(&format!("device key: {}\n", device.key_domain()));
            }
            if config.database_path.exists() {
                let db = open_database(config, false).await?;
                let federation = db.federation_store();
                let certs = stored_certs(&federation).await;
                let mut lines = Vec::new();
                if let Ok(certs) = &certs {
                    for cert in certs {
                        let revoked = stored_revocations(&federation, &cert.person)
                            .await
                            .ok()
                            .flatten()
                            .is_some_and(|list| list.contains(&cert.device));
                        lines.push(format!(
                            "  {} of {}{}{}{}\n",
                            cert.device.to_z32(),
                            cert.person.to_z32(),
                            if cert.name.is_empty() {
                                String::new()
                            } else {
                                format!(" \"{}\"", cert.name)
                            },
                            if cert.admin { " admin" } else { "" },
                            if revoked { " REVOKED" } else { "" },
                        ));
                    }
                }
                db.close().await;
                certs?;
                if !lines.is_empty() {
                    out.push_str("devices known:\n");
                    out.extend(lines);
                }
            }
            Ok(out)
        }
    }
}

/// The output of the `person pair` commands.
async fn pair(config: &Config, command: PairCommand) -> Result<String, Box<dyn std::error::Error>> {
    use resonator_federation::control::{
        config_rendezvous, offline_cancel, offline_invite, offline_join,
    };
    let device = ServerKey::load_or_create(&config.federation_key_path())?;
    let db = open_database(config, true).await?;
    let store = db.federation_store();
    let result: Result<String, Box<dyn std::error::Error>> = async {
        match command {
            PairCommand::Invite { admin } => {
                let invitation = offline_invite(
                    config,
                    store.clone(),
                    device.public(),
                    admin,
                    config_rendezvous(config),
                )
                .await?;
                let code = match &invitation.code {
                    Some(code) => format!(
                        "pairing code (valid for 10 minutes, once; the node must be running):\n\n  \
                         {code}\n\non the new device: resonator person pair join {code}\n\nor the "
                    ),
                    None => "no pairing code (the pairing rendezvous is off or cannot be \
                             reached); the "
                        .to_owned(),
                };
                Ok(format!(
                    "{code}pairing link (valid for 10 minutes, once; the node must be running):\n\n  \
                     {}\n\non the new device: resonator person pair join <link>{}\n",
                    invitation.link,
                    if admin {
                        "\nthen confirm here: resonator person pair confirm ".to_owned()
                            + &invitation.pending.id
                    } else {
                        String::new()
                    }
                ))
            }
            PairCommand::List => {
                let mut out = String::new();
                for record in store.pair_invitations().await? {
                    if record.expires_at <= now_millis() {
                        continue;
                    }
                    out.push_str(&format!(
                        "{} {}{} {}\n",
                        record.id,
                        record.state,
                        if record.admin { " admin" } else { "" },
                        record.joiner_name.unwrap_or_default()
                    ));
                }
                if out.is_empty() {
                    out.push_str("no open invitations\n");
                }
                Ok(out)
            }
            PairCommand::Confirm { id } => {
                if store
                    .update_pair_invitation(&id, "requested", "confirmed", None, None)
                    .await?
                {
                    Ok(format!("confirmed {id}\n"))
                } else {
                    Err(format!("no device waits for confirmation of {id}").into())
                }
            }
            PairCommand::Cancel { id } => {
                if offline_cancel(
                    config,
                    store.clone(),
                    device.public(),
                    &id,
                    config_rendezvous(config),
                )
                .await?
                {
                    Ok(format!("withdrew {id}\n"))
                } else {
                    Err(format!("no invitation {id}").into())
                }
            }
            PairCommand::Join {
                invitation,
                name,
                wait,
            } => {
                let progress = |stage: resonator_federation::pairing::JoinStage| {
                    use resonator_federation::pairing::JoinStage;
                    let text = match stage {
                        JoinStage::Finding => "finding the inviting device",
                        JoinStage::Verifying => "verifying",
                        JoinStage::Waiting => "waiting for confirmation on the inviting device",
                        JoinStage::Done => "paired",
                    };
                    eprintln!("{text}...");
                };
                let summary = offline_join(
                    config,
                    device.secret().clone(),
                    &store,
                    &invitation.join(" "),
                    &name,
                    std::time::Duration::from_secs(wait),
                    config_rendezvous(config),
                    &progress,
                )
                .await?;
                Ok(format!(
                    "paired: this device belongs to {}{}; start the node\n",
                    summary.person,
                    if summary.admin { " (admin)" } else { "" }
                ))
            }
        }
    }
    .await;
    db.close().await;
    result
}

/// The output of `federation key`.
fn federation_key(config: &Config) -> Result<String, Box<dyn std::error::Error>> {
    let path = config.federation_key_path();
    let key = ServerKey::load_or_create(&path)?;
    let key_domain = key.key_domain();
    let mut out = format!("key: {key_domain}\nfile: {}\n", path.display());
    if config.domain == key_domain {
        out.push_str("domain: the key domain itself; no DNS record is needed\n");
    } else {
        out.push_str(&format!(
            "DNS record binding {} to this key:\n{}\n",
            config.domain,
            resonator_federation::dns::zone_line(&config.domain, &[key.public()])
        ));
    }
    if !config.federation.enabled {
        out.push_str("federation is disabled; set [federation] enabled = true to use it\n");
    }
    Ok(out)
}

/// The output of `federation peers [approve|deny <peer>]`.
async fn federation_peers(
    config: &Config,
    action: Option<PeersAction>,
) -> Result<String, Box<dyn std::error::Error>> {
    use resonator_federation::admin::{Decision, decide, format_peers};
    use resonator_server::storage::Database;
    let path = config.database_path.clone();
    if !path.exists() {
        return Err(format!("database {} does not exist", path.display()).into());
    }
    let options = config.database_options();
    let db = tokio::task::spawn_blocking(move || Database::open(path, options)).await??;
    let store = db.federation_store();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        });
    let decision = match action {
        None => None,
        Some(PeersAction::Approve { peer }) => Some((peer, Decision::Approve)),
        Some(PeersAction::Deny { peer }) => Some((peer, Decision::Deny)),
    };
    let out: Result<String, Box<dyn std::error::Error>> = match decision {
        None => store
            .peers()
            .await
            .map(|peers| format_peers(&peers))
            .map_err(Into::into),
        Some((peer, decision)) => decide(&store, &peer, decision, now)
            .await
            .map(|changed| format_peers(&changed))
            .map_err(Into::into),
    };
    db.close().await;
    out
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("resonator: {error}");
            ExitCode::FAILURE
        }
    }
}
