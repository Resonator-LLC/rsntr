//! The peers test guest: it lists its peers and what reached it.
//!
//! - render: node 1 is the number of peers, node 2 what reached it
//!   (`;`-separated), node 3 a "ping" button, node 4 a "streams" button
//! - ping: sends "ping" to every peer; a peer answers "pong"
//! - streams: opens a reliable and a datagram stream to every peer and
//!   writes "r:hi" and "d:hi"; the other end answers "r:back" / "d:back"
//!   on the same stream and closes the reliable one

use std::cell::RefCell;

wit_bindgen::generate!({
    path: "../../../wit",
    world: "wave-app",
});

use resonator::app::{peers, ui};

#[derive(Default)]
struct State {
    peers: Vec<Peer>,
    seen: Vec<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn seen(what: String) {
    STATE.with_borrow_mut(|s| s.seen.push(what));
    ui::invalidate(0);
}

fn node(parent: Option<u32>, tag: &str, text: Option<String>, on: &[&str]) -> UiNode {
    UiNode {
        parent,
        tag: tag.into(),
        text,
        attrs: vec![],
        on: on.iter().map(|s| s.to_string()).collect(),
    }
}

struct App;

impl Guest for App {
    fn init(_app_id: String, _self_peer: Peer) {}

    fn on_messages(_messages: Vec<WaveMessage>) {}

    fn on_peers_changed(list: Vec<Peer>) {
        STATE.with_borrow_mut(|s| s.peers = list);
        ui::invalidate(0);
    }

    fn on_peer_data(from: String, data: Vec<u8>) {
        let text = String::from_utf8_lossy(&data).into_owned();
        if text == "ping" {
            let _ = peers::send(&from, b"pong");
        }
        seen(text);
    }

    fn on_stream(_stream: u64, _from: String, mode: StreamMode) {
        seen(match mode {
            StreamMode::Reliable => "open:r".into(),
            StreamMode::Datagram => "open:d".into(),
        });
    }

    fn on_stream_data(stream: u64, data: Vec<u8>) {
        let text = String::from_utf8_lossy(&data).into_owned();
        match text.as_str() {
            "r:hi" => {
                let _ = peers::write(stream, b"r:back");
                peers::close(stream);
            }
            "d:hi" => {
                let _ = peers::write(stream, b"d:back");
            }
            _ => {}
        }
        seen(text);
    }

    fn on_stream_closed(_stream: u64) {
        seen("closed".into());
    }

    fn render(_view: u32) -> Vec<UiNode> {
        STATE.with_borrow(|s| {
            vec![
                node(None, "div", None, &[]),
                node(Some(0), "span", Some(s.peers.len().to_string()), &[]),
                node(Some(0), "span", Some(s.seen.join(";")), &[]),
                node(Some(0), "button", Some("ping".into()), &["click"]),
                node(Some(0), "button", Some("streams".into()), &["click"]),
            ]
        })
    }

    fn on_ui(_view: u32, event: UiEvent) {
        let peers = STATE.with_borrow(|s| s.peers.clone());
        match event.node {
            3 => {
                for peer in &peers {
                    let _ = peers::send(&peer.id, b"ping");
                }
            }
            4 => {
                for peer in &peers {
                    if let Ok(stream) = peers::open(&peer.id, StreamMode::Reliable) {
                        let _ = peers::write(stream, b"r:hi");
                    }
                    if let Ok(stream) = peers::open(&peer.id, StreamMode::Datagram) {
                        let _ = peers::write(stream, b"d:hi");
                    }
                }
            }
            _ => {}
        }
    }
}

export!(App);
