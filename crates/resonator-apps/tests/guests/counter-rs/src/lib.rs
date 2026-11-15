//! The counter test guest. Its behavior is mirrored exactly by counter-js;
//! the runtime tests check that both give the same renders and effects.
//!
//! - click on the button: count + 1, kv "count", post ("inc", count), redraw
//! - an "inc" message from the wave: count = max(count, body)
//! - ui event "spin": loops forever (deadline test)
//! - ui event "hog": grows memory until it fails (limit test)

use std::cell::RefCell;

wit_bindgen::generate!({
    path: "../../../wit",
    world: "wave-app",
});

use resonator::app::{kv, log, ui, wave};

struct State {
    count: u64,
    error: Option<String>,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State { count: 0, error: None }) };
}

struct Counter;

fn attrs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

impl Guest for Counter {
    fn init(_app_id: String, _self_peer: Peer) {
        let stored = kv::get("count")
            .and_then(|v| String::from_utf8(v).ok())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        STATE.with_borrow_mut(|s| s.count = stored);
        log::log(log::Level::Info, "counter started");
    }

    fn on_messages(messages: Vec<WaveMessage>) {
        let mut changed = false;
        STATE.with_borrow_mut(|s| {
            for m in messages.iter().filter(|m| m.kind == "inc") {
                if let Ok(n) = m.body.parse::<u64>()
                    && n > s.count
                {
                    s.count = n;
                    changed = true;
                }
            }
        });
        if changed {
            ui::invalidate(0);
        }
    }

    fn on_peers_changed(_peers: Vec<Peer>) {}
    fn on_peer_data(_from: String, _data: Vec<u8>) {}
    fn on_stream(_stream: u64, _from: String, _mode: StreamMode) {}
    fn on_stream_data(_stream: u64, _data: Vec<u8>) {}
    fn on_stream_closed(_stream: u64) {}

    fn render(_view: u32) -> Vec<UiNode> {
        STATE.with_borrow(|s| {
            let mut nodes = vec![
                UiNode {
                    parent: None,
                    tag: "div".into(),
                    text: None,
                    attrs: attrs(&[("class", "app-row")]),
                    on: vec![],
                },
                UiNode {
                    parent: Some(0),
                    tag: "span".into(),
                    text: Some(s.count.to_string()),
                    attrs: vec![],
                    on: vec![],
                },
                UiNode {
                    parent: Some(0),
                    tag: "button".into(),
                    text: Some("+1".into()),
                    attrs: vec![],
                    on: vec!["click".into()],
                },
            ];
            if let Some(e) = &s.error {
                nodes.push(UiNode {
                    parent: Some(0),
                    tag: "p".into(),
                    text: Some(e.clone()),
                    attrs: attrs(&[("class", "app-error")]),
                    on: vec![],
                });
            }
            nodes
        })
    }

    fn on_ui(_view: u32, event: UiEvent) {
        match event.name.as_str() {
            "click" if event.node == 2 => {
                let count = STATE.with_borrow_mut(|s| {
                    s.count += 1;
                    s.count
                });
                let body = count.to_string();
                let mut errors = Vec::new();
                if let Err(e) = kv::set("count", body.as_bytes()) {
                    errors.push(format!("kv: {e}"));
                }
                if let Err(e) = wave::post("inc", &body) {
                    errors.push(format!("wave: {e}"));
                }
                STATE
                    .with_borrow_mut(|s| s.error = (!errors.is_empty()).then(|| errors.join("; ")));
                ui::invalidate(0);
            }
            "spin" => {
                let mut x: u64 = 0;
                loop {
                    x = std::hint::black_box(x.wrapping_add(1));
                }
            }
            "hog" => {
                let mut blocks: Vec<Vec<u8>> = Vec::new();
                loop {
                    blocks.push(vec![1u8; 1 << 20]);
                    std::hint::black_box(&blocks);
                }
            }
            _ => {}
        }
    }
}

export!(Counter);
