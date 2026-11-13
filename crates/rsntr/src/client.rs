//! The server's HTTP API as `rsntr` uses it: signing in for an access
//! token, then the MCP tools (`POST /mcp`, JSON-RPC, stateless) with it.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::config::Account;

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("rsntr/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

/// The server of an address: `https://<its domain>`.
pub fn server_of(address: &str) -> Result<String> {
    match address.split_once('@') {
        Some((name, domain)) if !name.is_empty() && !domain.is_empty() => {
            Ok(format!("https://{domain}"))
        }
        _ => bail!("{address:?} is not an address like you@rsntr.net"),
    }
}

/// Signs in to `server` and makes an access token named `name` that can
/// read and write.
pub async fn sign_in(server: &str, address: &str, password: &str, name: &str) -> Result<String> {
    let http = http()?;
    let response = http
        .post(format!("{server}/auth/signin"))
        .json(&json!({"address": address, "password": password}))
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    match response.status().as_u16() {
        200 => {}
        401 | 403 => bail!("wrong address or password"),
        status => bail!("{server} answered the sign-in with {status}"),
    }
    let cookie = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .collect::<Vec<_>>()
        .join("; ");
    if cookie.is_empty() {
        bail!("{server} signed in without a session");
    }
    let response = http
        .post(format!("{server}/auth/tokens"))
        .header(reqwest::header::COOKIE, cookie)
        // Token management checks that the request comes from the server's
        // own pages.
        .header(reqwest::header::ORIGIN, server)
        .json(&json!({"name": name, "scopes": ["read", "write"]}))
        .send()
        .await?;
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if status != 201 {
        bail!(
            "{server} did not make a token ({status}): {}",
            body["message"].as_str().unwrap_or("no reason given")
        );
    }
    body["token"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{server} made a token but did not return it"))
}

/// The result of a tool: its text, and whether the tool failed.
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

/// Calls the MCP tool `name` with `arguments`.
pub async fn call(account: &Account, name: &str, arguments: Value) -> Result<ToolResult> {
    let server = &account.server;
    let response = http()?
        .post(format!("{server}/mcp"))
        .bearer_auth(&account.token)
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }))
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    match response.status().as_u16() {
        200 => {}
        401 => bail!("the access token was refused: run `rsntr login` again"),
        403 => bail!("the access token may not do this"),
        404 => bail!("{server} has no MCP endpoint (agents are off there)"),
        status => bail!("{server} answered {status}"),
    }
    let body: Value = response
        .json()
        .await
        .with_context(|| format!("{server} sent something that is not JSON"))?;
    if let Some(error) = body.get("error") {
        bail!("{}", error["message"].as_str().unwrap_or("the call failed"));
    }
    let result = &body["result"];
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolResult {
        text,
        is_error: result["isError"].as_bool().unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_addresss_server_is_its_domain_over_https() {
        assert_eq!(server_of("me@rsntr.net").unwrap(), "https://rsntr.net");
        assert!(server_of("me").is_err());
        assert!(server_of("@rsntr.net").is_err());
        assert!(server_of("me@").is_err());
    }
}
