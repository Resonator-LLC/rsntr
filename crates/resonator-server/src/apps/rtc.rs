//! App media over WebRTC between a browser and this node.
//!
//! One task owns the UDP socket and every peer connection (str0m, which
//! does no I/O of its own); packets find their connection by
//! [`Rtc::accepts`]. The node is ICE-lite with one candidate, the
//! configured `advertise` address.
//!
//! A connection belongs to one app box ([`RtcLink`], held by the box's
//! stream task). The browser sends its microphone (and camera) on
//! `sendonly` tracks; those frames go to the app as if they came over the
//! client socket. It receives on `recvonly` tracks, the slots: the app's
//! frames to the viewer (`[channel][...]`) go to the slot that carries
//! their channel, and [`RtcEvent::Slots`] says which slot carries which.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use str0m::change::SdpOffer;
use str0m::format::Codec;
use str0m::media::{Direction, Frequency, KeyframeRequestKind, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};

use super::{AppSession, MAX_VIDEO_FRAME};
use crate::config::WebRtcConfig;

/// Most peer connections at once on this node.
const MAX_PEERS: usize = 256;
/// Most bytes of one microphone frame.
const MAX_AUDIO_FRAME: usize = 4096;
/// How often the node asks the browser for a key frame of its camera,
/// so viewers elsewhere (who cannot ask) see a picture soon.
const KEY_FRAME_EVERY: Duration = Duration::from_secs(2);
/// A slot without frames for this long carries no channel any more.
const SLOT_IDLE: Duration = Duration::from_secs(3);
/// Housekeeping: key frame requests and idle slots.
const TICK: Duration = Duration::from_millis(250);
/// Samples of one Opus frame at 48 kHz (20 ms).
const OPUS_FRAME: u64 = 960;

/// What a link hears from its connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtcEvent {
    /// Media flows now; the app's frames go over the connection.
    Connected,
    /// Which slot (by mid) carries which app channel, all of them.
    Slots(Vec<(String, u8)>),
}

#[derive(Debug)]
enum Command {
    Accept {
        offer: SdpOffer,
        session: Arc<AppSession>,
        events: mpsc::UnboundedSender<RtcEvent>,
        reply: oneshot::Sender<Result<(u64, String), String>>,
    },
    Send {
        id: u64,
        frame: Vec<u8>,
        video: bool,
    },
    Close(u64),
}

/// The WebRTC side of this node; cheap to clone.
#[derive(Debug, Clone)]
pub struct RtcHub {
    commands: mpsc::Sender<Command>,
    local: SocketAddr,
}

impl RtcHub {
    /// Binds the UDP socket and starts the task. Needs a tokio runtime.
    pub fn start(config: &WebRtcConfig) -> Result<Self, String> {
        tokio::runtime::Handle::try_current()
            .map_err(|_| "WebRTC needs a tokio runtime".to_owned())?;
        let advertise = config
            .advertise
            .ok_or_else(|| "webrtc.advertise is not set".to_owned())?;
        let socket = std::net::UdpSocket::bind(config.listen)
            .map_err(|e| format!("WebRTC socket {}: {e}", config.listen))?;
        socket
            .set_nonblocking(true)
            .map_err(|e| format!("WebRTC socket: {e}"))?;
        let local = socket
            .local_addr()
            .map_err(|e| format!("WebRTC socket: {e}"))?;
        let socket = UdpSocket::from_std(socket).map_err(|e| format!("WebRTC socket: {e}"))?;
        let (commands, receiver) = mpsc::channel(1024);
        tokio::spawn(drive(socket, advertise, receiver));
        tracing::info!(%local, %advertise, "WebRTC for app media");
        Ok(Self { commands, local })
    }

    /// The bound UDP address (tests listen on port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Answers a browser's offer for an app box; the link carries the
    /// box's media from now on.
    pub async fn accept(
        &self,
        offer: &str,
        session: Arc<AppSession>,
    ) -> Result<(String, RtcLink), String> {
        let offer = SdpOffer::from_sdp_string(offer).map_err(|e| format!("invalid offer: {e}"))?;
        let (reply, answer) = oneshot::channel();
        let (events, receiver) = mpsc::unbounded_channel();
        let stopped = || "WebRTC stopped".to_owned();
        self.commands
            .send(Command::Accept {
                offer,
                session,
                events,
                reply,
            })
            .await
            .map_err(|_| stopped())?;
        let (id, answer) = answer.await.map_err(|_| stopped())??;
        Ok((
            answer,
            RtcLink {
                id,
                commands: self.commands.clone(),
                events: receiver,
            },
        ))
    }
}

/// One app box's connection. Dropping it closes the connection.
#[derive(Debug)]
pub struct RtcLink {
    id: u64,
    commands: mpsc::Sender<Command>,
    events: mpsc::UnboundedReceiver<RtcEvent>,
}

impl RtcLink {
    /// Sends a frame the app wrote to the viewer (`[channel][...]`);
    /// dropped when the task is behind.
    pub fn send(&self, frame: Vec<u8>, video: bool) {
        let _ = self.commands.try_send(Command::Send {
            id: self.id,
            frame,
            video,
        });
    }

    /// The next event; `None` once the connection is gone.
    pub async fn event(&mut self) -> Option<RtcEvent> {
        self.events.recv().await
    }
}

impl Drop for RtcLink {
    fn drop(&mut self) {
        // When the queue is full the peer still goes: its events have no
        // receiver any more, which the tick notices.
        let _ = self.commands.try_send(Command::Close(self.id));
    }
}

async fn drive(socket: UdpSocket, advertise: SocketAddr, mut commands: mpsc::Receiver<Command>) {
    let mut peers: Vec<Peer> = Vec::new();
    let mut next_id = 0u64;
    let mut buf = vec![0u8; 2000];
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let deadline = peers
            .iter()
            .map(|p| p.timeout)
            .min()
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            received = socket.recv_from(&mut buf) => {
                let Ok((n, source)) = received else { continue };
                let now = Instant::now();
                let Ok(contents) = buf[..n].try_into() else { continue };
                let input = Input::Receive(now, Receive {
                    proto: Protocol::Udp,
                    source,
                    destination: advertise,
                    contents,
                });
                if let Some(peer) = peers.iter_mut().find(|p| p.rtc.accepts(&input)) {
                    if peer.rtc.handle_input(input).is_err() {
                        peer.rtc.disconnect();
                    }
                    peer.drain(&socket, now);
                }
            }
            command = commands.recv() => {
                let Some(command) = command else { return };
                let now = Instant::now();
                match command {
                    Command::Accept { offer, session, events, reply } => {
                        let answer = if peers.len() >= MAX_PEERS {
                            Err("Too many WebRTC connections on this node".to_owned())
                        } else {
                            next_id += 1;
                            Peer::accept(next_id, offer, session, events, advertise, &socket, now)
                                .map(|peer| {
                                    let answer = peer.1.clone();
                                    peers.push(peer.0);
                                    (next_id, answer)
                                })
                        };
                        let _ = reply.send(answer);
                    }
                    Command::Send { id, frame, video } => {
                        if let Some(peer) = peers.iter_mut().find(|p| p.id == id) {
                            peer.send(&frame, video, now);
                            peer.drain(&socket, now);
                        }
                    }
                    Command::Close(id) => {
                        if let Some(peer) = peers.iter_mut().find(|p| p.id == id) {
                            peer.rtc.disconnect();
                        }
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline.into()) => {
                let now = Instant::now();
                for peer in peers.iter_mut().filter(|p| p.timeout <= now) {
                    if peer.rtc.handle_input(Input::Timeout(now)).is_err() {
                        peer.rtc.disconnect();
                    }
                    peer.drain(&socket, now);
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                for peer in &mut peers {
                    peer.housekeeping(now);
                    peer.drain(&socket, now);
                }
            }
        }
        peers.retain(|p| p.rtc.is_alive());
    }
}

/// A receiving track of the browser and the channel it carries.
struct Slot {
    mid: Mid,
    channel: Option<u8>,
    used: Instant,
    /// Media time of the last frame written.
    time: u64,
    /// Video: frames wait for a key frame (a new channel, or the browser
    /// lost the picture).
    waiting_key: bool,
}

impl Slot {
    /// Whether a video frame may go out: after a key frame, all do.
    fn admit(&mut self, key: bool) -> bool {
        if self.waiting_key && !key {
            return false;
        }
        self.waiting_key = false;
        true
    }
}

/// The slot for an app channel: the one that carries it, else a free one,
/// else the one used longest ago, which then waits for a key frame. With
/// whether the slots changed; `None` when there are none.
fn slot_for(slots: &mut [Slot], channel: u8) -> Option<(usize, bool)> {
    if let Some(index) = slots.iter().position(|s| s.channel == Some(channel)) {
        return Some((index, false));
    }
    let index = slots
        .iter()
        .enumerate()
        .min_by_key(|(_, s)| (s.channel.is_some(), s.used))
        .map(|(index, _)| index)?;
    slots[index].channel = Some(channel);
    slots[index].waiting_key = true;
    Some((index, true))
}

struct Peer {
    id: u64,
    rtc: Rtc,
    session: Arc<AppSession>,
    events: mpsc::UnboundedSender<RtcEvent>,
    timeout: Instant,
    started: Instant,
    connected: bool,
    /// The browser's camera track, and when a frame of it last came.
    camera: Option<(Mid, Option<Instant>)>,
    last_key_request: Option<Instant>,
    audio: Vec<Slot>,
    video: Vec<Slot>,
    slots_changed: bool,
}

impl Peer {
    fn accept(
        id: u64,
        offer: SdpOffer,
        session: Arc<AppSession>,
        events: mpsc::UnboundedSender<RtcEvent>,
        advertise: SocketAddr,
        socket: &UdpSocket,
        now: Instant,
    ) -> Result<(Peer, String), String> {
        let rtc = Rtc::builder()
            .set_ice_lite(true)
            .clear_codecs()
            .enable_opus(true, false)
            .enable_vp8(true)
            .enable_h264(true)
            .build(now);
        let mut peer = Peer {
            id,
            rtc,
            session,
            events,
            timeout: now,
            started: now,
            connected: false,
            camera: None,
            last_key_request: None,
            audio: Vec::new(),
            video: Vec::new(),
            slots_changed: false,
        };
        let candidate =
            Candidate::host(advertise, "udp").map_err(|e| format!("WebRTC candidate: {e}"))?;
        peer.rtc.add_local_candidate(candidate);
        peer.drain(socket, now);
        let answer = peer
            .rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|e| format!("offer refused: {e}"))?;
        peer.drain(socket, now);
        Ok((peer, answer.to_sdp_string()))
    }

    /// Polls str0m until it only has a timeout left.
    fn drain(&mut self, socket: &UdpSocket, now: Instant) {
        loop {
            match self.rtc.poll_output() {
                Ok(Output::Timeout(at)) => {
                    self.timeout = at;
                    break;
                }
                Ok(Output::Transmit(transmit)) => {
                    let _ = socket.try_send_to(&transmit.contents, transmit.destination);
                }
                Ok(Output::Event(event)) => self.event(event, now),
                Err(error) => {
                    tracing::debug!(%error, "WebRTC connection failed");
                    self.rtc.disconnect();
                    self.timeout = now + Duration::from_secs(1);
                    break;
                }
            }
        }
        if std::mem::take(&mut self.slots_changed) {
            let slots = self
                .audio
                .iter()
                .chain(&self.video)
                .filter_map(|s| s.channel.map(|c| (s.mid.to_string(), c)))
                .collect();
            let _ = self.events.send(RtcEvent::Slots(slots));
        }
    }

    fn event(&mut self, event: Event, now: Instant) {
        match event {
            Event::IceConnectionStateChange(state) => match state {
                IceConnectionState::Connected | IceConnectionState::Completed => {
                    if !std::mem::replace(&mut self.connected, true) {
                        let _ = self.events.send(RtcEvent::Connected);
                    }
                }
                IceConnectionState::Disconnected => self.rtc.disconnect(),
                _ => {}
            },
            Event::MediaAdded(added) => {
                let slot = |mid| Slot {
                    mid,
                    channel: None,
                    used: now,
                    time: 0,
                    waiting_key: true,
                };
                match (added.kind, added.direction) {
                    (MediaKind::Video, Direction::RecvOnly) => {
                        self.camera = Some((added.mid, None));
                    }
                    (MediaKind::Audio, Direction::SendOnly) => self.audio.push(slot(added.mid)),
                    (MediaKind::Video, Direction::SendOnly) => self.video.push(slot(added.mid)),
                    _ => {}
                }
            }
            Event::MediaData(data) => {
                let codec = data.params.spec().codec;
                if codec == Codec::Opus {
                    if data.data.len() <= MAX_AUDIO_FRAME {
                        let _ = self.session.media(data.data.to_vec(), false);
                    }
                    return;
                }
                let bits = match codec {
                    Codec::Vp8 => 0u8,
                    Codec::H264 => 1,
                    _ => return,
                };
                if let Some((_, last)) = &mut self.camera {
                    *last = Some(now);
                }
                if !data.contiguous {
                    self.request_key_frame(now);
                }
                if data.data.len() + 1 > MAX_VIDEO_FRAME {
                    return;
                }
                let mut frame = Vec::with_capacity(data.data.len() + 1);
                frame.push(u8::from(data.is_keyframe()) | (bits << 1));
                frame.extend_from_slice(&data.data);
                let _ = self.session.media(frame, true);
            }
            Event::KeyframeRequest(request) => {
                if let Some(slot) = self.video.iter_mut().find(|s| s.mid == request.mid) {
                    slot.waiting_key = true;
                }
            }
            _ => {}
        }
    }

    fn request_key_frame(&mut self, now: Instant) {
        let Some((mid, _)) = self.camera else { return };
        if let Some(mut writer) = self.rtc.writer(mid)
            && writer
                .request_keyframe(None, KeyframeRequestKind::Pli)
                .is_ok()
        {
            self.last_key_request = Some(now);
        }
    }

    fn housekeeping(&mut self, now: Instant) {
        if self.events.is_closed() {
            self.rtc.disconnect();
            return;
        }
        // While the camera sends, a key frame every so often.
        if let Some((_, Some(last))) = self.camera
            && now.duration_since(last) < Duration::from_secs(1)
            && self
                .last_key_request
                .is_none_or(|at| now.duration_since(at) >= KEY_FRAME_EVERY)
        {
            self.request_key_frame(now);
        }
        for slot in self.audio.iter_mut().chain(&mut self.video) {
            if slot.channel.is_some() && now.duration_since(slot.used) >= SLOT_IDLE {
                slot.channel = None;
                self.slots_changed = true;
            }
        }
    }

    /// Writes a frame of the app (`[channel][...]`, video
    /// `[channel][flags][encoded]`) to the slot of its channel.
    fn send(&mut self, frame: &[u8], video: bool, now: Instant) {
        if !self.connected {
            return;
        }
        let Some((&channel, rest)) = frame.split_first() else {
            return;
        };
        let (codec, key, data) = if video {
            let Some((&flags, data)) = rest.split_first() else {
                return;
            };
            let codec = match (flags >> 1) & 7 {
                0 => Codec::Vp8,
                1 => Codec::H264,
                _ => return,
            };
            (codec, flags & 1 == 1, data)
        } else {
            (Codec::Opus, true, rest)
        };
        if data.is_empty() {
            return;
        }
        let slots = if video {
            &mut self.video
        } else {
            &mut self.audio
        };
        let Some((index, changed)) = slot_for(slots, channel) else {
            return;
        };
        self.slots_changed |= changed;
        let slot = &mut slots[index];
        slot.used = now;
        if video && !slot.admit(key) {
            return;
        }
        let (frequency, step) = if video {
            (Frequency::NINETY_KHZ, 1)
        } else {
            (Frequency::FORTY_EIGHT_KHZ, OPUS_FRAME)
        };
        // Arrival time, but never behind the last frame plus its length:
        // frames that come in a burst keep their spacing.
        let elapsed = now.duration_since(self.started).as_micros();
        let ticks =
            u64::try_from(elapsed * u128::from(frequency.get()) / 1_000_000).unwrap_or(u64::MAX);
        slot.time = ticks.max(slot.time + step);
        let (mid, time) = (slot.mid, MediaTime::new(slot.time, frequency));
        let Some(writer) = self.rtc.writer(mid) else {
            return;
        };
        let Some(pt) = writer
            .payload_params()
            .find(|p| p.spec().codec == codec)
            .map(|p| p.pt())
        else {
            return;
        };
        if let Err(error) = writer.write(pt, now, time, data.to_vec()) {
            tracing::debug!(%error, "WebRTC write failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(n: usize, now: Instant) -> Vec<Slot> {
        (0..n)
            .map(|i| Slot {
                mid: Mid::from(i.to_string().as_str()),
                channel: None,
                used: now,
                time: 0,
                waiting_key: true,
            })
            .collect()
    }

    #[test]
    fn a_channel_keeps_its_slot_and_the_oldest_slot_goes_first() {
        let start = Instant::now();
        let mut slots = slots(2, start);
        assert_eq!(slot_for(&mut slots, 7), Some((0, true)));
        assert_eq!(slot_for(&mut slots, 7), Some((0, false)));
        assert_eq!(slot_for(&mut slots, 9), Some((1, true)));
        // Both taken: channel 7 was used longest ago, so 3 takes its slot.
        slots[0].used = start + Duration::from_millis(1);
        slots[1].used = start + Duration::from_millis(2);
        assert_eq!(slot_for(&mut slots, 3), Some((0, true)));
        assert_eq!(slots[0].channel, Some(3));
        assert_eq!(slot_for(&mut [], 3), None);
    }

    #[test]
    fn a_taken_over_slot_waits_for_a_key_frame() {
        let mut slots = slots(1, Instant::now());
        slot_for(&mut slots, 1);
        assert!(!slots[0].admit(false));
        assert!(slots[0].admit(true));
        assert!(slots[0].admit(false));
        slot_for(&mut slots, 2);
        assert!(!slots[0].admit(false));
    }
}
