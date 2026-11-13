//! `rsntr`: Resonator from the terminal. Signs in to any Resonator server
//! and reads and writes waves through its MCP tools.

mod client;
mod config;

use std::io::{IsTerminal, Read};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use config::Account;

#[derive(Parser)]
#[command(
    name = "rsntr",
    version,
    about = "Resonator from the terminal: read and write waves on any Resonator server",
    after_help = "Sign in once with `rsntr login you@rsntr.net`. Scripts and agents can \
                  skip that and set RSNTR_SERVER and RSNTR_TOKEN instead.\n\
                  Wave ids look like rsntr.net/w+abc123; on your own server `abc123` is enough."
)]
struct Cli {
    /// Print the server's JSON instead of a summary.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in and keep an access token for this computer.
    Login {
        /// Your address, like you@rsntr.net.
        address: String,
        /// The server, if not https://<your domain>.
        #[arg(long)]
        server: Option<String>,
        /// Read the password from stdin instead of asking for it.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Forget the access token kept on this computer.
    Logout,
    /// Show the account in use.
    Whoami,
    /// The waves in your inbox.
    Inbox {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: u32,
    },
    /// The waves with something you have not read.
    Unread {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: u32,
    },
    /// Search waves, with the web client's queries (in:inbox, with:bob@...,
    /// words).
    Search {
        #[arg(required = true)]
        query: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: u32,
    },
    /// Read a wave.
    Read {
        #[arg(allow_hyphen_values = true)]
        wave: String,
        /// Mark it read afterwards.
        #[arg(long)]
        mark_read: bool,
    },
    /// Start a wave.
    New {
        title: String,
        /// The text of the first blip (else stdin, if it is not a terminal).
        #[arg(short, long)]
        message: Option<String>,
        /// Someone to add; repeat for more.
        #[arg(short, long = "with")]
        with: Vec<String>,
    },
    /// Write in a wave: at its end, or as a reply to one of its blips.
    Reply {
        #[arg(allow_hyphen_values = true)]
        wave: String,
        /// The blip to reply to (from `rsntr read`).
        #[arg(allow_hyphen_values = true)]
        blip: Option<String>,
        /// The text (else stdin, if it is not a terminal).
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Rename a wave.
    Title {
        #[arg(allow_hyphen_values = true)]
        wave: String,
        title: String,
    },
    /// Add someone to a wave.
    Add {
        #[arg(allow_hyphen_values = true)]
        wave: String,
        address: String,
    },
    /// Remove someone from a wave.
    Remove {
        #[arg(allow_hyphen_values = true)]
        wave: String,
        address: String,
    },
    /// Mark a wave read.
    MarkRead {
        #[arg(allow_hyphen_values = true)]
        wave: String,
    },
    /// Call any of the server's MCP tools with JSON arguments.
    Tool {
        name: String,
        #[arg(default_value = "{}")]
        arguments: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("rsntr: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("rsntr: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let path = config::path()?;
    let account = || config::load(&path);
    let json = cli.json;
    let (tool, arguments, account) = match cli.command {
        Command::Login {
            address,
            server,
            password_stdin,
        } => {
            let server = match server {
                Some(server) => server.trim_end_matches('/').to_owned(),
                None => client::server_of(&address)?,
            };
            let password = if password_stdin {
                let mut password = String::new();
                std::io::stdin().read_to_string(&mut password)?;
                password.trim_end_matches(['\r', '\n']).to_owned()
            } else {
                rpassword::prompt_password(format!("Password for {address}: "))?
            };
            let token = client::sign_in(&server, &address, &password, &token_name()).await?;
            config::save(
                &path,
                &Account {
                    server: server.clone(),
                    address: address.clone(),
                    token,
                },
            )?;
            println!("Signed in to {server} as {address}.");
            return Ok(ExitCode::SUCCESS);
        }
        Command::Logout => {
            if config::remove(&path)? {
                println!(
                    "Forgot the access token. Revoke it in the web client's settings to end it \
                     on the server too."
                );
            } else {
                println!("Not signed in.");
            }
            return Ok(ExitCode::SUCCESS);
        }
        Command::Whoami => {
            let account = account()?;
            if account.address.is_empty() {
                println!("A token for {} (from RSNTR_TOKEN)", account.server);
            } else {
                println!("{} on {}", account.address, account.server);
            }
            return Ok(ExitCode::SUCCESS);
        }
        Command::Inbox { limit } => (
            "search_waves",
            json!({"query": "in:inbox", "limit": limit}),
            account()?,
        ),
        Command::Unread { limit } => ("list_unread", json!({"limit": limit}), account()?),
        Command::Search { query, limit } => (
            "search_waves",
            json!({"query": query.join(" "), "limit": limit}),
            account()?,
        ),
        Command::Read { wave, mark_read } => {
            let account = account()?;
            let wave = wave_id(&account, &wave);
            let code = show(&account, "read_wave", json!({"wave_id": wave}), json).await?;
            if mark_read && code == ExitCode::SUCCESS {
                let result = client::call(&account, "mark_read", json!({"wave_id": wave})).await?;
                if result.is_error {
                    bail!("{}", result.text);
                }
            }
            return Ok(code);
        }
        Command::New {
            title,
            message,
            with,
        } => {
            let mut arguments = json!({"title": title, "participants": with});
            if let Some(text) = text(message, false)? {
                arguments["text"] = Value::String(text);
            }
            ("create_wave", arguments, account()?)
        }
        Command::Reply {
            wave,
            blip,
            message,
        } => {
            let account = account()?;
            let wave = wave_id(&account, &wave);
            let text = text(message, true)?.unwrap_or_default();
            match blip {
                Some(blip) => (
                    "reply_to_blip",
                    json!({"wave_id": wave, "blip_id": blip_id(&blip), "text": text}),
                    account,
                ),
                None => (
                    "append_blip",
                    json!({"wave_id": wave, "text": text}),
                    account,
                ),
            }
        }
        Command::Title { wave, title } => {
            let account = account()?;
            let wave = wave_id(&account, &wave);
            (
                "set_title",
                json!({"wave_id": wave, "title": title}),
                account,
            )
        }
        Command::Add { wave, address } => {
            let account = account()?;
            let wave = wave_id(&account, &wave);
            (
                "add_participant",
                json!({"wave_id": wave, "address": address}),
                account,
            )
        }
        Command::Remove { wave, address } => {
            let account = account()?;
            let wave = wave_id(&account, &wave);
            (
                "remove_participant",
                json!({"wave_id": wave, "address": address}),
                account,
            )
        }
        Command::MarkRead { wave } => {
            let account = account()?;
            let wave = wave_id(&account, &wave);
            ("mark_read", json!({"wave_id": wave}), account)
        }
        Command::Tool { name, arguments } => {
            let arguments: Value =
                serde_json::from_str(&arguments).context("the arguments are not JSON")?;
            let account = account()?;
            return show(&account, &name, arguments, true).await;
        }
    };
    show(&account, tool, arguments, json).await
}

/// Calls a tool and prints its result: as the server sent it with
/// `--json` (or when it is not JSON), else summarised.
async fn show(account: &Account, tool: &str, arguments: Value, json: bool) -> Result<ExitCode> {
    let result = client::call(account, tool, arguments).await?;
    if result.is_error {
        eprintln!("rsntr: {}", result.text);
        return Ok(ExitCode::FAILURE);
    }
    match serde_json::from_str::<Value>(&result.text) {
        Ok(value) if !json => print!("{}", summary(&value)),
        _ => println!("{}", result.text),
    }
    Ok(ExitCode::SUCCESS)
}

/// A tool's JSON result for people: a line per wave of a list, the new
/// ids of a write.
fn summary(value: &Value) -> String {
    let mut out = String::new();
    match value {
        Value::Array(waves) if waves.is_empty() => out.push_str("No waves.\n"),
        Value::Array(waves) => {
            for wave in waves {
                let title = match wave["title"].as_str() {
                    Some(title) if !title.is_empty() => title,
                    _ => wave["snippet"].as_str().unwrap_or(""),
                };
                let unread = wave["unread"].as_u64().unwrap_or(0);
                let mark = if unread > 0 {
                    format!("{unread:>3} new")
                } else {
                    "       ".to_owned()
                };
                out.push_str(&format!(
                    "{mark}  {:<40}  {}\n",
                    clip(title, 40),
                    wave["wave_id"].as_str().unwrap_or("")
                ));
            }
        }
        Value::Object(fields) if fields.is_empty() => out.push_str("Done.\n"),
        Value::Object(fields) => {
            for (key, value) in fields {
                let value = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned);
                out.push_str(&format!("{key}: {value}\n"));
            }
        }
        other => out.push_str(&format!("{other}\n")),
    }
    out
}

/// At most `width` characters of one line of `text`.
fn clip(text: &str, width: usize) -> String {
    let line = text.lines().next().unwrap_or("");
    if line.chars().count() <= width {
        return line.to_owned();
    }
    let mut clipped: String = line.chars().take(width - 3).collect();
    clipped.push_str("...");
    clipped
}

/// A wave id in full: `abc123` and `w+abc123` are waves of the account's
/// own domain.
fn wave_id(account: &Account, wave: &str) -> String {
    if wave.contains('/') {
        return wave.to_owned();
    }
    let wave = wave.strip_prefix("w+").unwrap_or(wave);
    format!("{}/w+{wave}", account.domain())
}

/// A blip id in full: `abc` is `b+abc`.
fn blip_id(blip: &str) -> String {
    if blip.starts_with("b+") {
        blip.to_owned()
    } else {
        format!("b+{blip}")
    }
}

/// The text to write: `message`, else stdin when it is not a terminal.
fn text(message: Option<String>, required: bool) -> Result<Option<String>> {
    if message.is_some() {
        return Ok(message);
    }
    if std::io::stdin().is_terminal() {
        if required {
            bail!("give the text with -m, or on stdin");
        }
        return Ok(None);
    }
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    let text = text.trim_end().to_owned();
    if text.is_empty() && required {
        bail!("the text is empty");
    }
    Ok((!text.is_empty()).then_some(text))
}

/// The access token's name, as listed in the web client: rsntr and this
/// computer.
fn token_name() -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .filter(|host| !host.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|host| host.trim().to_owned())
                .filter(|host| !host.is_empty())
        });
    match host {
        Some(host) => format!("rsntr on {host}"),
        None => "rsntr".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Account {
        Account {
            server: "https://rsntr.net".to_owned(),
            address: "me@rsntr.net".to_owned(),
            token: String::new(),
        }
    }

    #[test]
    fn short_wave_ids_are_of_the_accounts_domain() {
        let account = account();
        assert_eq!(wave_id(&account, "abc"), "rsntr.net/w+abc");
        assert_eq!(wave_id(&account, "w+abc"), "rsntr.net/w+abc");
        assert_eq!(
            wave_id(&account, "resonator.network/w+abc"),
            "resonator.network/w+abc"
        );
        assert_eq!(blip_id("xyz"), "b+xyz");
        assert_eq!(blip_id("b+xyz"), "b+xyz");
    }

    #[test]
    fn lists_summarise_to_a_line_per_wave() {
        let waves = json!([
            {"title": "", "snippet": "Hello there\nsecond line", "unread": 2,
             "wave_id": "rsntr.net/w+a"},
            {"title": "Plans", "snippet": "x", "unread": 0, "wave_id": "rsntr.net/w+b"},
        ]);
        let text = summary(&waves);
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("  2 new  Hello there "), "{text}");
        assert!(lines[0].ends_with("rsntr.net/w+a"));
        assert!(lines[1].starts_with("         Plans "), "{text}");
        assert_eq!(summary(&json!([])), "No waves.\n");
        assert_eq!(
            summary(&json!({"wave_id": "rsntr.net/w+a"})),
            "wave_id: rsntr.net/w+a\n"
        );
        assert_eq!(clip(&"x".repeat(50), 10), "xxxxxxx...");
    }
}
