//! Helpers that build and edit conversation waves on a running server with
//! the model's own editing layers, as a client would.
#![allow(dead_code)]

use std::time::{SystemTime, UNIX_EPOCH};

use resonator_model::conversation::{WaveletBasedConversation, make_wavelet_conversational};
use resonator_model::data::WaveletData;
use resonator_model::docop::{Attributes, DocInitialization, DocOpBuilder};
use resonator_model::id::util::build_user_data_wavelet_id;
use resonator_model::id::{IdGeneratorImpl, ParticipantId, WaveId, WaveletId, WaveletName};
use resonator_model::supplement::{
    DefaultFollow, SupplementedWaveImpl, WaveletBasedSupplement, WaveletDataView,
};
use resonator_model::version::HashedVersionFactory;
use resonator_model::wavelet::{OpBasedWavelet, WaveView};
use resonator_server::waveserver::{WaveServer, WaveletProvider};

use crate::common::DOMAIN;

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
}

pub fn wave_id(id: &str) -> WaveId {
    WaveId::of(DOMAIN, id).expect("wave id")
}

pub fn root_name(wave: &WaveId) -> WaveletName {
    WaveletName::of(
        wave.clone(),
        WaveletId::of(wave.domain(), "conv+root").expect("wavelet id"),
    )
}

/// A blip document with one line of text.
pub fn blip_content(text: &str) -> DocInitialization {
    DocOpBuilder::new()
        .element_start("body", Attributes::new())
        .element_start("line", Attributes::new())
        .element_end()
        .characters(text)
        .element_end()
        .build_initialization()
        .expect("blip content")
}

fn id_generator(seed: &str) -> IdGeneratorImpl {
    let seed = seed.to_owned();
    IdGeneratorImpl::new(DOMAIN, move || seed.clone())
}

/// Creates a wave with a root conversation whose first blip holds `text`
/// (its first line becomes the title), with `others` as further
/// participants.
pub async fn create_wave(
    server: &WaveServer,
    author: &ParticipantId,
    id: &str,
    others: &[&ParticipantId],
    text: &str,
) -> WaveId {
    let wave = wave_id(id);
    let mut ids = id_generator(&format!("{id}a"));
    let mut view = WaveView::new(wave.clone(), author.clone(), now());
    let root = view.create_root().expect("root wavelet");
    make_wavelet_conversational(root).expect("manifest");
    let mut conversation = WaveletBasedConversation::new(root, &mut ids).expect("conversation");
    conversation
        .append_blip_with_content("", Some(&blip_content(text)))
        .expect("root blip");
    conversation.update_implicit_title().expect("title");
    for other in others {
        conversation.add_participant(other).expect("participant");
    }
    for (name, delta) in view.take_deltas() {
        server.submit_delta(&name, &delta).await.expect("submit");
    }
    wave
}

/// The current state of a wavelet.
pub async fn wavelet(server: &WaveServer, name: &WaveletName) -> WaveletData {
    server
        .get_snapshot(name)
        .await
        .expect("snapshot")
        .unwrap_or_else(|| panic!("no wavelet {name}"))
        .snapshot
}

/// Edits the root wavelet of a wave as `author` and submits the result.
pub async fn edit_root(
    server: &WaveServer,
    author: &ParticipantId,
    wave: &WaveId,
    edit: impl FnOnce(&mut WaveletBasedConversation<'_>),
) {
    let name = root_name(wave);
    let data = wavelet(server, &name).await;
    let seed = format!("{}v{}", wave.id(), data.version());
    let mut ids = id_generator(&seed);
    let mut root = OpBasedWavelet::new(data, author.clone(), now());
    {
        let mut conversation =
            WaveletBasedConversation::new(&mut root, &mut ids).expect("conversation");
        edit(&mut conversation);
    }
    if let Some(delta) = root.take_delta() {
        server.submit_delta(&name, &delta).await.expect("submit");
    }
}

/// Appends a blip with `text` to the root thread.
pub async fn append_blip(server: &WaveServer, author: &ParticipantId, wave: &WaveId, text: &str) {
    edit_root(server, author, wave, |conversation| {
        conversation
            .append_blip_with_content("", Some(&blip_content(text)))
            .expect("blip");
    })
    .await;
}

/// Changes `user`'s supplement of a wave (read state, archive, follow) and
/// submits the change to their user-data wavelet, creating it if need be.
pub async fn edit_supplement(
    server: &WaveServer,
    user: &ParticipantId,
    wave: &WaveId,
    edit: impl FnOnce(&mut SupplementedWaveImpl<WaveletBasedSupplement<'_>, WaveletDataView<'_>>),
) {
    let root = wavelet(server, &root_name(wave)).await;
    let udw_name = WaveletName::of(
        wave.clone(),
        build_user_data_wavelet_id(user).expect("user data wavelet id"),
    );
    let existing = server.get_snapshot(&udw_name).await.expect("snapshot");
    let mut udw = match existing {
        Some(committed) => OpBasedWavelet::new(committed.snapshot, user.clone(), now()),
        None => {
            let data = WaveletData::empty(
                &udw_name,
                user.clone(),
                HashedVersionFactory::create_version_zero(&udw_name),
                now(),
            );
            let mut udw = OpBasedWavelet::new(data, user.clone(), now());
            udw.add_participant(user).expect("participant");
            udw
        }
    };
    {
        let view = WaveletDataView::new(user.clone(), [&root]);
        let state = WaveletBasedSupplement::create(&mut udw).expect("supplement");
        let mut supplement = SupplementedWaveImpl::new(state, view, DefaultFollow::Always);
        edit(&mut supplement);
    }
    if let Some(delta) = udw.take_delta() {
        server
            .submit_delta(&udw_name, &delta)
            .await
            .expect("submit");
    }
}

pub async fn mark_read(server: &WaveServer, user: &ParticipantId, wave: &WaveId) {
    edit_supplement(server, user, wave, |s| s.mark_as_read().expect("mark read")).await;
}

pub async fn archive(server: &WaveServer, user: &ParticipantId, wave: &WaveId) {
    edit_supplement(server, user, wave, |s| s.archive().expect("archive")).await;
}

pub async fn mute(server: &WaveServer, user: &ParticipantId, wave: &WaveId) {
    edit_supplement(server, user, wave, |s| s.mute().expect("mute")).await;
}

pub async fn follow(server: &WaveServer, user: &ParticipantId, wave: &WaveId) {
    edit_supplement(server, user, wave, |s| s.follow().expect("follow")).await;
}
