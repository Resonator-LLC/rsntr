//! A shared counter: everyone in the wave sees the same count. Each click
//! posts the new count to the wave; every instance takes the highest it
//! has seen, and keeps it in kv for the next start.

use std::cell::Cell;

use resonator_app_sdk::{
    Guest, Peer, StreamMode, UiEvent, UiNode, View, WaveMessage, export, kv, kv_text, ui, wave,
};

thread_local! {
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

struct Counter;

impl Guest for Counter {
    fn init(_app_id: String, _self_peer: Peer) {
        let stored = kv_text("count").and_then(|s| s.parse().ok()).unwrap_or(0);
        COUNT.set(stored);
    }

    fn on_messages(messages: Vec<WaveMessage>) {
        let highest = messages
            .iter()
            .filter(|m| m.kind == "inc")
            .filter_map(|m| m.body.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        if highest > COUNT.get() {
            COUNT.set(highest);
            let _ = kv::set("count", highest.to_string().as_bytes());
            ui::invalidate(0);
        }
    }

    fn on_peers_changed(_peers: Vec<Peer>) {}
    fn on_peer_data(_from: String, _data: Vec<u8>) {}
    fn on_stream(_stream: u64, _from: String, _mode: StreamMode) {}
    fn on_stream_data(_stream: u64, _data: Vec<u8>) {}
    fn on_stream_closed(_stream: u64) {}

    fn render(_view: u32) -> Vec<UiNode> {
        let mut v = View::new();
        let row = v.el(None, "div").class("app-row").id();
        v.el(Some(row), "span").class("app-large app-bold").text(COUNT.get().to_string());
        v.el(Some(row), "button").text("+1").on("click");
        v.finish()
    }

    fn on_ui(_view: u32, event: UiEvent) {
        if event.name != "click" {
            return;
        }
        let count = COUNT.get() + 1;
        COUNT.set(count);
        let body = count.to_string();
        let _ = kv::set("count", body.as_bytes());
        let _ = wave::post("inc", &body);
        ui::invalidate(0);
    }
}

export!(Counter);
