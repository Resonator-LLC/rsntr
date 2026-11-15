//! Walkie with video: everyone in the wave who has the app open hears and
//! sees whoever turned their microphone or camera on.
//!
//! The viewer (the peer `viewer`) hands the app two streams: the
//! microphone as a datagram stream (Opus frames) and the camera as a
//! reliable stream (a flags byte, then an encoded video frame). The app
//! sends each frame to every other peer, sound on a datagram stream and
//! video on a reliable one, so the mode of an incoming stream says which
//! it is. What it hears and sees goes back to the viewer prefixed with a
//! channel byte, one per person: sound on a datagram stream to `viewer`,
//! video on a reliable one. Echo sends your own back to you, to test.

use std::cell::RefCell;
use std::collections::HashMap;

use resonator_app_sdk::{
    Guest, Peer, StreamMode, UiEvent, UiNode, View, WaveMessage, export, peers, ui,
};

const VIEWER: &str = "viewer";

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Sound,
    Video,
}

impl Kind {
    fn of(mode: StreamMode) -> Kind {
        match mode {
            StreamMode::Datagram => Kind::Sound,
            StreamMode::Reliable => Kind::Video,
        }
    }

    fn mode(self) -> StreamMode {
        match self {
            Kind::Sound => StreamMode::Datagram,
            Kind::Video => StreamMode::Reliable,
        }
    }
}

#[derive(Default)]
struct Counts {
    sent: u64,
    got: u64,
}

#[derive(Default)]
struct State {
    peers: Vec<Peer>,
    /// Our streams to each peer, by peer id and kind.
    out: HashMap<(String, Kind), u64>,
    /// Streams coming in: who is on them, and what they carry.
    incoming: HashMap<u64, (String, Kind)>,
    /// The viewer's microphone and camera.
    from_viewer: HashMap<u64, Kind>,
    /// Our streams to the viewer's speaker and screen.
    to_viewer: HashMap<Kind, u64>,
    /// Channel numbers of people (0 is the echo).
    channels: HashMap<String, u8>,
    echo: bool,
    sound: Counts,
    video: Counts,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn to_viewer(state: &mut State, kind: Kind, channel: u8, frame: &[u8]) {
    if !state.to_viewer.contains_key(&kind)
        && let Ok(stream) = peers::open(VIEWER, kind.mode())
    {
        state.to_viewer.insert(kind, stream);
    }
    if let Some(stream) = state.to_viewer.get(&kind) {
        let mut data = Vec::with_capacity(frame.len() + 1);
        data.push(channel);
        data.extend_from_slice(frame);
        let _ = peers::write(*stream, &data);
    }
}

fn channel_of(state: &mut State, peer: &str) -> u8 {
    let next = (state.channels.len() as u8).saturating_add(1);
    *state.channels.entry(peer.to_owned()).or_insert(next)
}

fn counts(state: &mut State, kind: Kind) -> &mut Counts {
    match kind {
        Kind::Sound => &mut state.sound,
        Kind::Video => &mut state.video,
    }
}

struct WalkieCam;

impl Guest for WalkieCam {
    fn init(_app_id: String, _self_peer: Peer) {}

    fn on_messages(_messages: Vec<WaveMessage>) {}

    fn on_peers_changed(list: Vec<Peer>) {
        STATE.with_borrow_mut(|s| {
            let others: Vec<&Peer> = list.iter().filter(|p| !p.local).collect();
            s.out.retain(|(id, _), stream| {
                let keep = others.iter().any(|p| &p.id == id);
                if !keep {
                    peers::close(*stream);
                }
                keep
            });
            for peer in others {
                for kind in [Kind::Sound, Kind::Video] {
                    let key = (peer.id.clone(), kind);
                    if !s.out.contains_key(&key)
                        && let Ok(stream) = peers::open(&peer.id, kind.mode())
                    {
                        s.out.insert(key, stream);
                    }
                }
            }
            s.peers = list;
        });
        ui::invalidate(0);
    }

    fn on_peer_data(_from: String, _data: Vec<u8>) {}

    fn on_stream(stream: u64, from: String, mode: StreamMode) {
        STATE.with_borrow_mut(|s| {
            let kind = Kind::of(mode);
            if from == VIEWER {
                s.from_viewer.insert(stream, kind);
            } else {
                s.incoming.insert(stream, (from, kind));
            }
        });
    }

    fn on_stream_data(stream: u64, data: Vec<u8>) {
        STATE.with_borrow_mut(|s| {
            if let Some(&kind) = s.from_viewer.get(&stream) {
                counts(s, kind).sent += 1;
                for ((_, out_kind), out) in &s.out {
                    if *out_kind == kind {
                        let _ = peers::write(*out, &data);
                    }
                }
                if s.echo {
                    to_viewer(s, kind, 0, &data);
                }
            } else if let Some((from, kind)) = s.incoming.get(&stream).cloned() {
                counts(s, kind).got += 1;
                let channel = channel_of(s, &from);
                to_viewer(s, kind, channel, &data);
            }
            // Redraw now and then, not for every frame.
            let total = s.sound.sent + s.sound.got + s.video.sent + s.video.got;
            if total % 25 == 1 {
                ui::invalidate(0);
            }
        });
    }

    fn on_stream_closed(stream: u64) {
        STATE.with_borrow_mut(|s| {
            s.from_viewer.remove(&stream);
            s.to_viewer.retain(|_, out| *out != stream);
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
                .text("Use the microphone and camera buttons above.");
            let row = v.el(Some(col), "div").class("app-row").id();
            v.el(Some(row), "span").text(format!(
                "voice: sent {}, heard {}",
                s.sound.sent, s.sound.got
            ));
            v.el(Some(row), "span").text(format!(
                "video: sent {}, seen {}",
                s.video.sent, s.video.got
            ));
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

export!(WalkieCam);
