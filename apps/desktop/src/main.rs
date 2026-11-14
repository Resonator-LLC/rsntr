//! The Resonator app for macOS, Windows and Linux: a window on a node that
//! runs inside the app (crates/resonator-ffi, as on phones). The node's
//! pages stay in the window; other links open in the browser.

// No console window next to the app on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use resonator_ffi::{NodeConfig, ResonatorNode};
use tauri::{AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

/// The iroh relay the node keeps a home on: the official one, unless the
/// build sets `RESONATOR_IROH_RELAY` (empty: iroh's public relays).
const IROH_RELAY: &str = match option_env!("RESONATOR_IROH_RELAY") {
    Some(relay) => relay,
    None => "https://rly.rsntr.net",
};

/// The running node, stopped when the app quits.
#[derive(Default)]
struct Node(Mutex<Option<Arc<ResonatorNode>>>);

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Node::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let opener = handle.clone();
            let window =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title("Resonator")
                    .inner_size(1200.0, 820.0)
                    .min_inner_size(380.0, 480.0)
                    // Ctrl/Cmd + and - zoom the page (WCAG 1.4.4).
                    .zoom_hotkeys_enabled(true)
                    .on_navigation(move |url| {
                        if stays_in_app(url) {
                            return true;
                        }
                        open_outside(&opener, url);
                        false
                    })
                    .build()?;
            let data_dir = app.path().app_data_dir()?.join("node");
            std::thread::spawn(move || start(&handle, &window, data_dir));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("the app could not start");
    app.run(|app, event| {
        if let RunEvent::Exit = event {
            let node = app
                .state::<Node>()
                .0
                .lock()
                .ok()
                .and_then(|mut node| node.take());
            if let Some(node) = node {
                node.stop();
            }
        }
    });
}

/// Starts the node (it blocks until it serves) and opens its signed-in
/// page in the window.
fn start(app: &AppHandle, window: &WebviewWindow, data_dir: std::path::PathBuf) {
    let result = std::fs::create_dir_all(&data_dir)
        .map_err(|error| format!("cannot create {}: {error}", data_dir.display()))
        .and_then(|()| {
            ResonatorNode::start(NodeConfig {
                data_dir: data_dir.to_string_lossy().into_owned(),
                account: None,
                device_name: Some(gethostname::gethostname().to_string_lossy().into_owned()),
                allow_remote: false,
                extra_toml: extra_toml(),
            })
            .map_err(|error| error.to_string())
        });
    let node = match result {
        Ok(node) => node,
        Err(error) => return show(window, &format!("The node did not start: {error}")),
    };
    let Some(url) = node.signed_in_url().and_then(|url| Url::parse(&url).ok()) else {
        return show(window, "The node gave no address to open.");
    };
    if let Ok(mut slot) = app.state::<Node>().0.lock() {
        *slot = Some(node);
    }
    if let Err(error) = window.navigate(url) {
        show(window, &format!("Cannot open the node's page: {error}"));
    }
}

/// Settings laid over the device defaults.
fn extra_toml() -> Option<String> {
    let relay = IROH_RELAY.trim();
    (!relay.is_empty() && !relay.contains('"'))
        .then(|| format!("[federation]\nrelay = \"{relay}\"\n"))
}

/// The app's own pages and the node's (on loopback) stay in the window.
fn stays_in_app(url: &Url) -> bool {
    match url.scheme() {
        "tauri" | "about" | "data" | "blob" => true,
        "http" | "https" => matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "tauri.localhost")
        ),
        _ => false,
    }
}

/// Opens a link in the system's browser (or mail app); only web and mail
/// links.
fn open_outside(app: &AppHandle, url: &Url) {
    if matches!(url.scheme(), "http" | "https" | "mailto") {
        let _ = app.opener().open_url(url.as_str(), None::<&str>);
    }
}

/// Shows a message on the start page.
fn show(window: &WebviewWindow, text: &str) {
    let text = serde_json::to_string(text).unwrap_or_default();
    let _ = window.eval(format!(
        "document.getElementById('status').textContent = {text};"
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_apps_and_the_nodes_pages_stay_in_the_window() {
        for url in [
            "tauri://localhost/index.html",
            "http://tauri.localhost/index.html",
            "http://127.0.0.1:53211/?token=x",
            "about:blank",
        ] {
            assert!(stays_in_app(&Url::parse(url).unwrap()), "{url}");
        }
        for url in [
            "https://rsntr.net/",
            "https://example.com/127.0.0.1",
            "mailto:me@rsntr.net",
            "file:///etc/passwd",
        ] {
            assert!(!stays_in_app(&Url::parse(url).unwrap()), "{url}");
        }
    }

    #[test]
    fn the_relay_is_a_federation_setting() {
        assert_eq!(
            extra_toml().as_deref(),
            Some("[federation]\nrelay = \"https://rly.rsntr.net\"\n")
        );
    }
}
