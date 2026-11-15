//! Push to talk with everyone in the wave who has the app open.
//!
//! The viewer's microphone reaches the app as a datagram stream from the
//! peer `viewer` (Opus frames, one per datagram). The app sends each frame
//! to every other peer on a datagram stream of its own, and hands what it
//! hears to the viewer on a stream it opens to `viewer`, each frame
//! prefixed with a channel byte (one per speaker; the viewer keeps one
//! decoder per channel). Echo sends your own voice back to you, to test.

use std::cell::RefCell;
use std::collections::HashMap;

use resonator_app_sdk::{
    Guest, Peer, StreamMode, UiEvent, UiNode, View, WaveMessage, export, peers, ui,
};

const VIEWER: &str = "viewer";

#[derive(Default)]
struct State {
    peers: Vec<Peer>,
    /// Our stream to each peer, by peer id.
    out: HashMap<String, u64>,
    /// Incoming streams: who is on them.
    incoming: HashMap<u64, String>,
    /// The viewer's microphone.
    mic: Option<u64>,
    /// Our stream to the viewer's speaker.
    speaker: Option<u64>,
    /// Channel numbers of speakers (0 is the echo).
    channels: HashMap<String, u8>,
    echo: bool,
    sent: u64,
    heard: u64,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn to_speaker(state: &mut State, channel: u8, frame: &[u8]) {
    if state.speaker.is_none() {
        state.speaker = peers::open(VIEWER, StreamMode::Datagram).ok();
    }
    if let Some(stream) = state.speaker {
        let mut data = Vec::with_capacity(frame.len() + 1);
        data.push(channel);
        data.extend_from_slice(frame);
        let _ = peers::write(stream, &data);
    }
}

fn channel_of(state: &mut State, peer: &str) -> u8 {
    let next = (state.channels.len() as u8).saturating_add(1);
    *state.channels.entry(peer.to_owned()).or_insert(next)
}

struct Walkie;

impl Guest for Walkie {
    fn init(_app_id: String, _self_peer: Peer) {}

    fn on_messages(_messages: Vec<WaveMessage>) {}

    fn on_peers_changed(list: Vec<Peer>) {
        STATE.with_borrow_mut(|s| {
            let others: Vec<&Peer> = list.iter().filter(|p| !p.local).collect();
            s.out.retain(|id, stream| {
                let keep = others.iter().any(|p| &p.id == id);
                if !keep {
                    peers::close(*stream);
                }
                keep
            });
            for peer in others {
                if !s.out.contains_key(&peer.id)
                    && let Ok(stream) = peers::open(&peer.id, StreamMode::Datagram)
                {
                    s.out.insert(peer.id.clone(), stream);
                }
            }
            s.peers = list;
        });
        ui::invalidate(0);
    }

    fn on_peer_data(_from: String, _data: Vec<u8>) {}

    fn on_stream(stream: u64, from: String, _mode: StreamMode) {
        STATE.with_borrow_mut(|s| {
            if from == VIEWER {
                s.mic = Some(stream);
            } else {
                s.incoming.insert(stream, from);
            }
        });
    }

    fn on_stream_data(stream: u64, data: Vec<u8>) {
        STATE.with_borrow_mut(|s| {
            if s.mic == Some(stream) {
                s.sent += 1;
                for out in s.out.values() {
                    let _ = peers::write(*out, &data);
                }
                if s.echo {
                    to_speaker(s, 0, &data);
                }
            } else if let Some(from) = s.incoming.get(&stream).cloned() {
                s.heard += 1;
                let channel = channel_of(s, &from);
                to_speaker(s, channel, &data);
            }
            // Redraw now and then, not for every frame.
            if (s.sent + s.heard) % 25 == 1 {
                ui::invalidate(0);
            }
        });
    }

    fn on_stream_closed(stream: u64) {
        STATE.with_borrow_mut(|s| {
            if s.mic == Some(stream) {
                s.mic = None;
            }
            if s.speaker == Some(stream) {
                s.speaker = None;
            }
            s.incoming.remove(&stream);
            s.out.retain(|_, out| *out != stream);
        });
        ui::invalidate(0);
    }

    fn render(_view: u32) -> Vec<UiNode> {
        STATE.with_borrow(|s| {
            let mut v = View::new();
            let col = v.el(None, "div").class("app-col").id();
            let others = s.peers.iter().filter(|p| !p.local).count();
            let who = match others {
                0 => "Nobody else has it open".to_owned(),
                1 => "1 other person".to_owned(),
                n => format!("{n} other people"),
            };
            v.el(Some(col), "p").text(who);
            v.el(Some(col), "p")
                .class("app-muted")
                .text("Use the microphone button above to talk.");
            let row = v.el(Some(col), "div").class("app-row").id();
            v.el(Some(row), "span").text(format!("sent {}", s.sent));
            v.el(Some(row), "span").text(format!("heard {}", s.heard));
            v.el(Some(row), "button")
                .text(if s.echo { "Echo: on" } else { "Echo: off" })
                .on("click");
            v.finish()
        })
    }

    fn on_ui(_view: u32, event: UiEvent) {
        if event.name == "click" {
            STATE.with_borrow_mut(|s| s.echo = !s.echo);
            ui::invalidate(0);
        }
    }
}

export!(Walkie);
