//! End-to-end tests: the real server on an ephemeral port, driven by a real
//! HTTP client and real WebSocket clients speaking the wave protocol.
//!
//! The HTTP cases port what applies of `AuthenticationServletTest`,
//! `SignOutServletTest`, `UserRegistrationServletTest` and
//! `FetchServletTest` of Apache Wave; the WebSocket cases cover the
//! envelope and stream semantics of `RpcTest` and `WaveClientRpcImplTest`.

mod common;
#[path = "common/net.rs"]
mod net;

use std::time::Duration;

use futures_util::SinkExt;
use resonator_model::data::WaveletData;
use resonator_model::id::{ParticipantId, WaveId, WaveletName};
use resonator_model::operation::{WaveletOperation, transform};
use resonator_model::proto::federation::ProtocolHashedVersion;
use resonator_model::proto::json::from_json;
use resonator_model::proto::rpc::RpcFinished;
use resonator_model::proto::waveclient_rpc::{
    DocumentSnapshot, ProtocolAuthenticationResult, ProtocolOpenRequest, ProtocolSubmitResponse,
    ProtocolWaveletUpdate, WaveViewSnapshot, WaveletSnapshot, WaveletVersion,
};
use resonator_model::proto::{
    deserialize_delta, deserialize_hashed_version, deserialize_wavelet, serialize_hashed_version,
};
use resonator_model::version::HashedVersion;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::protocol::Message;

use common::{DOMAIN, add_participant, doc_xml, insert, no_op, participant, wavelet_name};
use net::{
    Frame, TestServer, WaveClient, resulting_version, session_cookie, try_connect, version_zero,
    wire_name,
};

const WAVE: &str = "example.com/w+abc";

fn root() -> WaveletName {
    wavelet_name("w+abc", "conv+root")
}

fn user(name: &str) -> ParticipantId {
    participant(&format!("{name}@{DOMAIN}"))
}

fn known(wavelet: &str, version: &HashedVersion) -> Vec<WaveletVersion> {
    vec![WaveletVersion {
        wavelet_id: Some(format!("{DOMAIN}/{wavelet}")),
        hashed_version: Some(serialize_hashed_version(version).unwrap()),
    }]
}

/// Creates the root wavelet of the test wave with these participants (the
/// first is the creator); returns the version after.
async fn create_wave(client: &mut WaveClient, participants: &[&ParticipantId]) -> HashedVersion {
    let creator = participants[0];
    let ops = participants
        .iter()
        .map(|who| add_participant(creator, who))
        .collect();
    client
        .submit_ok(creator, &root(), &version_zero(&root()), ops, None)
        .await
}

fn versions(update: &ProtocolWaveletUpdate) -> Vec<i64> {
    update
        .applied_delta
        .iter()
        .map(|delta| delta.hashed_version.as_ref().unwrap().version.unwrap())
        .collect()
}

// ---- The main flow ----

#[tokio::test]
async fn register_sign_in_open_create_and_collaborate() {
    let server = TestServer::start().await;
    let (alice, bob) = (user("alice"), user("bob"));
    let alice_cookie = server.user("alice").await;
    let bob_cookie = server.user("bob").await;

    // Alice opens the (still empty) wave: a channel id, then the marker.
    let mut alice_ws = WaveClient::with_cookie(&server, &alice_cookie).await;
    let seq = alice_ws.send_open(WAVE, Vec::new()).await;
    let first = alice_ws.next_for(seq).await;
    let channel: ProtocolWaveletUpdate = first.decode();
    let channel_id = channel.channel_id.clone().unwrap();
    assert_eq!(
        first.text,
        format!(
            "{{\"messageType\":\"ProtocolWaveletUpdate\",\"sequenceNumber\":{seq},\"message\":\
             {{\"1\":\"example.com/w+abc/~/dummy+root\",\"2\":[],\"7\":\"{channel_id}\"}}}}"
        )
    );
    let marker = alice_ws.next_for(seq).await;
    assert_eq!(
        marker.text,
        format!(
            "{{\"messageType\":\"ProtocolWaveletUpdate\",\"sequenceNumber\":{seq},\"message\":\
             {{\"1\":\"example.com/w+abc/~/dummy+root\",\"2\":[],\"6\":true}}}}"
        )
    );
    let alice_view = net::View {
        seq,
        channel_id: channel_id.clone(),
        initial: vec![channel, marker.decode()],
    };

    // She creates the wave by submitting the first delta of its root
    // wavelet, naming her channel.
    let submit_seq = alice_ws
        .send(
            &resonator_model::proto::waveclient_rpc::ProtocolSubmitRequest {
                wavelet_name: Some(wire_name(&root())),
                delta: Some(common::delta(
                    &alice,
                    &version_zero(&root()),
                    vec![add_participant(&alice, &alice)],
                )),
                channel_id: Some(channel_id.clone()),
            },
        )
        .await;
    let response = alice_ws.next_for(submit_seq).await;
    let submitted: ProtocolSubmitResponse = response.decode();
    assert_eq!(submitted.operations_applied, Some(1));
    assert_eq!(submitted.error_message, None);
    let v1 =
        deserialize_hashed_version(submitted.hashed_version_after_application.as_ref().unwrap())
            .unwrap();
    assert_eq!(v1.version(), 1);
    assert!(response.text.starts_with(&format!(
        "{{\"messageType\":\"ProtocolSubmitResponse\",\"sequenceNumber\":{submit_seq},\
         \"message\":{{\"1\":1,\"3\":{{\"1\":1.0,\"2\":\""
    )));

    // Bob has the wave open but is no participant yet: he sees nothing.
    let mut bob_ws = WaveClient::with_cookie(&server, &bob_cookie).await;
    let bob_view = bob_ws.open(WAVE, Vec::new()).await;
    assert_eq!(bob_view.initial.len(), 2);
    assert!(bob_view.initial[0].snapshot.is_none());
    assert_ne!(bob_view.channel_id, channel_id);

    // Alice writes, then adds Bob.
    let v2 = alice_ws
        .submit_ok(
            &alice,
            &root(),
            &v1,
            vec![insert(&alice, "b+1", 0, 0, "hello")],
            Some(&channel_id),
        )
        .await;
    bob_ws.expect_silence().await;
    let v3 = alice_ws
        .submit_ok(
            &alice,
            &root(),
            &v2,
            vec![add_participant(&alice, &bob)],
            Some(&channel_id),
        )
        .await;

    // The wavelet comes into Bob's view as a snapshot...
    let update = bob_ws.next_update(&bob_view).await;
    assert_eq!(
        update.wavelet_name.as_deref(),
        Some(wire_name(&root()).as_str())
    );
    assert_eq!(
        update.channel_id.as_deref(),
        Some(bob_view.channel_id.as_str())
    );
    assert!(update.applied_delta.is_empty());
    assert_eq!(resulting_version(&update), v3);
    let snapshot = update.snapshot.as_ref().unwrap();
    assert_eq!(
        snapshot.wavelet_id.as_deref(),
        Some("example.com/conv+root")
    );
    assert_eq!(
        snapshot.participant_id,
        ["alice@example.com", "bob@example.com"]
    );
    assert_eq!(snapshot.creator.as_deref(), Some("alice@example.com"));
    let wave_id = WaveId::of(DOMAIN, "w+abc").unwrap();
    let mut bob_state = deserialize_wavelet(snapshot, &wave_id).unwrap();
    assert_eq!(doc_xml(&bob_state, "b+1"), "hello");

    // ...and then he receives live updates.
    let v4 = alice_ws
        .submit_ok(
            &alice,
            &root(),
            &v3,
            vec![insert(&alice, "b+1", 5, 5, " world")],
            Some(&channel_id),
        )
        .await;
    let update = bob_ws.next_update(&bob_view).await;
    assert!(update.snapshot.is_none());
    assert_eq!(versions(&update), [3]);
    assert_eq!(resulting_version(&update), v4);
    assert_eq!(
        update.applied_delta[0].author.as_deref(),
        Some("alice@example.com")
    );
    for op in deserialize_delta(&update.applied_delta[0]).unwrap().ops() {
        op.apply(&mut bob_state).unwrap();
    }
    assert_eq!(doc_xml(&bob_state, "b+1"), "hello world");

    // Bob replies; Alice receives his delta, and never her own ones.
    let v5 = bob_ws
        .submit_ok(
            &bob,
            &root(),
            &v4,
            vec![insert(&bob, "b+1", 11, 11, "!")],
            Some(&bob_view.channel_id),
        )
        .await;
    let update = alice_ws.next_update(&alice_view).await;
    assert_eq!(versions(&update), [4]);
    assert_eq!(resulting_version(&update), v5);
    assert_eq!(
        update.applied_delta[0].author.as_deref(),
        Some("bob@example.com")
    );
    alice_ws.expect_silence().await;
    bob_ws.expect_silence().await;

    // Commit notices arrived along the way; the server agrees on the text.
    let fetched = server
        .get(
            "/fetch/example.com/w+abc/~/conv+root/b+1",
            Some(&bob_cookie),
        )
        .await;
    assert_eq!(fetched.status(), 200);
    let document: DocumentSnapshot = from_json(&fetched.text().await.unwrap()).unwrap();
    assert_eq!(document.author.as_deref(), Some("alice@example.com"));
    assert_eq!(
        document.contributor,
        ["alice@example.com", "bob@example.com"]
    );
    assert_eq!(server.metric("open_subscriptions").await, 2);
    assert_eq!(server.metric("websocket_connections").await, 2);
    assert_eq!(server.metric("submits_total").await, 5);
    assert_eq!(server.metric("submit_errors_total").await, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn commit_notices_are_sent_on_the_view() {
    let server = TestServer::start().await;
    let alice = user("alice");
    let cookie = server.user("alice").await;
    let mut ws = WaveClient::with_cookie(&server, &cookie).await;
    let v1 = create_wave(&mut ws, &[&alice]).await;
    let view = ws.open(WAVE, Vec::new()).await;
    let snapshot = &view.wavelets()[0];
    assert!(snapshot.snapshot.is_some());
    assert!(snapshot.commit_notice.is_some());
    let v2 = ws
        .submit_ok(&alice, &root(), &v1, vec![no_op(&alice)], None)
        .await;
    // The delta (no channel id was named, so it is echoed), then the
    // notice that it is durable.
    let delta = ws.next_update(&view).await;
    assert_eq!(versions(&delta), [1]);
    assert_eq!(delta.commit_notice, None);
    loop {
        let notice: ProtocolWaveletUpdate = ws.next_for(view.seq).await.decode();
        assert!(notice.applied_delta.is_empty() && notice.snapshot.is_none());
        assert_eq!(notice.resulting_version, None);
        assert_eq!(notice.channel_id.as_deref(), Some(view.channel_id.as_str()));
        let committed = deserialize_hashed_version(notice.commit_notice.as_ref().unwrap()).unwrap();
        // The commit of version 1 may still have been on its way.
        if committed == v2 {
            break;
        }
        assert_eq!(committed, v1);
    }
    server.shutdown().await;
}

// ---- Concurrent editing ----

/// A client-side replica of a wavelet, kept in step with the server by
/// transforming against concurrent deltas.
struct Replica {
    state: WaveletData,
    version: HashedVersion,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_edits_from_two_clients_converge() {
    let server = TestServer::start().await;
    let (alice, bob) = (user("alice"), user("bob"));
    let alice_cookie = server.user("alice").await;
    let bob_cookie = server.user("bob").await;
    let mut alice_ws = WaveClient::with_cookie(&server, &alice_cookie).await;
    let mut bob_ws = WaveClient::with_cookie(&server, &bob_cookie).await;
    let v = create_wave(&mut alice_ws, &[&alice, &bob]).await;
    alice_ws
        .submit_ok(
            &alice,
            &root(),
            &v,
            vec![insert(&alice, "b+1", 0, 0, "0123456789")],
            None,
        )
        .await;

    let wave_id = WaveId::of(DOMAIN, "w+abc").unwrap();
    let alice_view = alice_ws.open(WAVE, Vec::new()).await;
    let bob_view = bob_ws.open(WAVE, Vec::new()).await;
    let replica = |view: &net::View| {
        let update = &view.wavelets()[0];
        Replica {
            state: deserialize_wavelet(update.snapshot.as_ref().unwrap(), &wave_id).unwrap(),
            version: resulting_version(update),
        }
    };
    let mut replicas = [replica(&alice_view), replica(&bob_view)];
    assert_eq!(replicas[0].version, replicas[1].version);

    for round in 0..25usize {
        // Both edit the same version of the document at once.
        let size = 10 + 2 * round;
        let ops = [
            insert(&alice, "b+1", size, round % (size + 1), "a"),
            insert(&bob, "b+1", size, (round * 7) % (size + 1), "b"),
        ];
        let target = replicas[0].version.clone();
        let wavelet = root();
        let (alice_response, bob_response) = tokio::join!(
            alice_ws.submit(
                &alice,
                &wavelet,
                &target,
                vec![ops[0].clone()],
                Some(&alice_view.channel_id)
            ),
            bob_ws.submit(
                &bob,
                &wavelet,
                &target,
                vec![ops[1].clone()],
                Some(&bob_view.channel_id)
            ),
        );
        let responses = [alice_response, bob_response];
        let theirs = [
            alice_ws.next_update(&alice_view).await,
            bob_ws.next_update(&bob_view).await,
        ];
        for (index, replica) in replicas.iter_mut().enumerate() {
            assert_eq!(responses[index].error_message, None);
            assert_eq!(responses[index].operations_applied, Some(1));
            // Each receives exactly the other's delta.
            let delta = &theirs[index].applied_delta[0];
            assert_eq!(theirs[index].applied_delta.len(), 1);
            let server_op: WaveletOperation = deserialize_delta(delta).unwrap().ops()[0].clone();
            let applied_at = delta.hashed_version.as_ref().unwrap().version.unwrap() as u64;
            let own = &ops[index];
            own.apply(&mut replica.state).unwrap();
            if applied_at == target.version() {
                // The other's delta was applied first, at the version this
                // client's own delta was made for: transform it over the
                // own delta, as a client does with its unacknowledged ops.
                let pair = transform(own, &server_op).unwrap();
                pair.server_op.apply(&mut replica.state).unwrap();
            } else {
                // The own delta was first; the server already transformed
                // the other's over it.
                assert_eq!(applied_at, target.version() + 1);
                server_op.apply(&mut replica.state).unwrap();
            }
            replica.version = resulting_version(&theirs[index]).max(
                deserialize_hashed_version(
                    responses[index]
                        .hashed_version_after_application
                        .as_ref()
                        .unwrap(),
                )
                .unwrap(),
            );
        }
        assert_eq!(replicas[0].version, replicas[1].version, "round {round}");
        assert_eq!(replicas[0].version.version(), target.version() + 2);
        assert_eq!(
            doc_xml(&replicas[0].state, "b+1"),
            doc_xml(&replicas[1].state, "b+1"),
            "round {round}"
        );
    }

    // And both agree with the server.
    let fetched = server
        .get("/fetch/example.com/w+abc/~/conv+root", Some(&alice_cookie))
        .await;
    let snapshot: WaveletSnapshot = from_json(&fetched.text().await.unwrap()).unwrap();
    let on_server = deserialize_wavelet(&snapshot, &wave_id).unwrap();
    let text = doc_xml(&on_server, "b+1");
    assert_eq!(text, doc_xml(&replicas[0].state, "b+1"));
    assert_eq!(text.len(), 10 + 50);
    assert_eq!(text.matches('a').count(), 25);
    assert_eq!(text.matches('b').count(), 25);
    assert_eq!(on_server.hashed_version(), &replicas[0].version);
    server.shutdown().await;
}

// ---- Reconnecting ----

#[tokio::test]
async fn reconnect_and_reopen_from_known_versions() {
    let server = TestServer::start().await;
    let (alice, bob) = (user("alice"), user("bob"));
    let alice_cookie = server.user("alice").await;
    let bob_cookie = server.user("bob").await;
    let mut alice_ws = WaveClient::with_cookie(&server, &alice_cookie).await;
    let mut bob_ws = WaveClient::with_cookie(&server, &bob_cookie).await;
    let v2 = create_wave(&mut alice_ws, &[&alice, &bob]).await;
    let view = alice_ws.open(WAVE, Vec::new()).await;
    assert_eq!(resulting_version(&view.wavelets()[0]), v2);

    // Alice's connection drops; Bob carries on.
    drop(alice_ws);
    server.await_metric("websocket_connections", 1).await;
    server.await_metric("open_subscriptions", 0).await;
    let v3 = bob_ws
        .submit_ok(
            &bob,
            &root(),
            &v2,
            vec![insert(&bob, "b+1", 0, 0, "one")],
            None,
        )
        .await;
    let v4 = bob_ws
        .submit_ok(
            &bob,
            &root(),
            &v3,
            vec![insert(&bob, "b+1", 3, 3, " two")],
            None,
        )
        .await;

    // She reconnects and reopens from the version she has: no snapshot,
    // but the version to resume from and the deltas she missed.
    let mut alice_ws = WaveClient::with_cookie(&server, &alice_cookie).await;
    let view = alice_ws.open(WAVE, known("conv+root", &v2)).await;
    assert_eq!(view.initial.len(), 3);
    let resync = &view.initial[0];
    assert!(resync.snapshot.is_none() && resync.applied_delta.is_empty());
    assert_eq!(resulting_version(resync), v2);
    assert!(resync.commit_notice.is_some());
    assert_eq!(resync.channel_id.as_deref(), Some(view.channel_id.as_str()));
    let missed = &view.initial[1];
    assert!(missed.snapshot.is_none());
    assert_eq!(versions(missed), [2, 3]);
    assert_eq!(resulting_version(missed), v4);
    assert_eq!(view.initial[2].marker, Some(true));
    // The stream continues live from there.
    let v5 = bob_ws
        .submit_ok(&bob, &root(), &v4, vec![no_op(&bob)], None)
        .await;
    let live = alice_ws.next_update(&view).await;
    assert_eq!(versions(&live), [4]);
    assert_eq!(resulting_version(&live), v5);

    // Reopening at the current version: just the resync message.
    let view = alice_ws.open(WAVE, known("conv+root", &v5)).await;
    assert_eq!(view.initial.len(), 2);
    assert_eq!(resulting_version(&view.initial[0]), v5);
    assert!(view.initial[0].snapshot.is_none() && view.initial[0].applied_delta.is_empty());

    // A version the server's history does not have (another hash): a
    // snapshot, from which the client has to start over.
    let bogus = HashedVersion::of(3, vec![7; 20]);
    let view = alice_ws.open(WAVE, known("conv+root", &bogus)).await;
    assert_eq!(view.initial.len(), 2);
    let snapshot = view.initial[0].snapshot.as_ref().expect("snapshot");
    assert_eq!(resulting_version(&view.initial[0]), v5);
    let wave_id = WaveId::of(DOMAIN, "w+abc").unwrap();
    assert_eq!(
        doc_xml(&deserialize_wavelet(snapshot, &wave_id).unwrap(), "b+1"),
        "one two"
    );

    // Malformed known versions fail the request, not the connection.
    let seq = alice_ws
        .send(&ProtocolOpenRequest {
            participant_id: Some("alice@example.com".to_owned()),
            wave_id: Some(WAVE.to_owned()),
            wavelet_id_prefix: vec![String::new()],
            known_wavelet: vec![WaveletVersion {
                wavelet_id: Some("not a wavelet id".to_owned()),
                hashed_version: Some(ProtocolHashedVersion {
                    version: Some(1),
                    history_hash: Some(Vec::new()),
                }),
            }],
        })
        .await;
    assert!(alice_ws.next_for(seq).await.failure().is_some());
    alice_ws.open(WAVE, Vec::new()).await;
    server.shutdown().await;
}

// ---- Access control ----

#[tokio::test]
async fn access_is_denied_to_non_participants() {
    let server = TestServer::start().await;
    let (alice, carol) = (user("alice"), user("carol"));
    let alice_cookie = server.user("alice").await;
    let carol_cookie = server.user("carol").await;
    let mut alice_ws = WaveClient::with_cookie(&server, &alice_cookie).await;
    let v1 = create_wave(&mut alice_ws, &[&alice]).await;
    alice_ws
        .submit_ok(
            &alice,
            &root(),
            &v1,
            vec![insert(&alice, "b+1", 0, 0, "secret")],
            None,
        )
        .await;

    // Carol's view of the wave is empty and stays so.
    let mut carol_ws = WaveClient::with_cookie(&server, &carol_cookie).await;
    let view = carol_ws.open(WAVE, Vec::new()).await;
    assert_eq!(view.initial.len(), 2);
    assert!(view.initial[0].snapshot.is_none());
    assert_eq!(
        view.initial[0].wavelet_name.as_deref(),
        Some("example.com/w+abc/~/dummy+root")
    );
    // Knowing a version does not help.
    let view2 = carol_ws.open(WAVE, known("conv+root", &v1)).await;
    assert_eq!(view2.initial.len(), 2);
    assert!(view2.initial[0].resulting_version.is_none());

    // She cannot write to it,
    let response = carol_ws
        .submit(&carol, &root(), &v1, vec![no_op(&carol)], None)
        .await;
    assert_eq!(response.operations_applied, Some(0));
    assert_eq!(
        response.error_message.as_deref(),
        Some(
            "carol@example.com is not a participant of \
             [WaveletName example.com/w+abc/example.com/conv+root]"
        )
    );
    assert_eq!(response.hashed_version_after_application, None);
    // not even by pretending to be Alice,
    let response = carol_ws
        .submit(&alice, &root(), &v1, vec![no_op(&alice)], None)
        .await;
    assert_eq!(
        response.error_message.as_deref(),
        Some("Author field on delta must match logged in user")
    );
    // and she cannot fetch it.
    for path in [
        "/fetch/example.com/w+abc",
        "/fetch/example.com/w+abc/~/conv+root",
        "/fetch/example.com/w+abc/~/conv+root/b+1",
    ] {
        assert_eq!(server.get(path, Some(&carol_cookie)).await.status(), 403);
        assert_eq!(server.get(path, None).await.status(), 403);
        assert_eq!(server.get(path, Some(&alice_cookie)).await.status(), 200);
    }
    carol_ws.expect_silence().await;

    // Nobody but Alice touches Alice's user-data wavelet.
    let alices_data = wavelet_name("w+abc", "user+alice@example.com");
    let response = carol_ws
        .submit(
            &carol,
            &alices_data,
            &version_zero(&alices_data),
            vec![add_participant(&carol, &carol)],
            None,
        )
        .await;
    assert_eq!(
        response.error_message.as_deref(),
        Some(
            "carol@example.com may not write the user data wavelet \
             [WaveletName example.com/w+abc/example.com/user+alice@example.com]"
        )
    );
    alice_ws
        .submit_ok(
            &alice,
            &alices_data,
            &version_zero(&alices_data),
            vec![add_participant(&alice, &alice)],
            None,
        )
        .await;

    // Without signing in, nothing works.
    let mut anonymous = WaveClient::connect(&server, &[]).await;
    let seq = anonymous.send_open(WAVE, Vec::new()).await;
    let refused = anonymous.next_for(seq).await;
    assert_eq!(refused.failure().as_deref(), Some("Not logged in"));
    assert_eq!(
        refused.text,
        format!(
            "{{\"messageType\":\"RpcFinished\",\"sequenceNumber\":{seq},\"message\":\
             {{\"1\":true,\"2\":\"Not logged in\"}}}}"
        )
    );
    let response = anonymous
        .submit(&alice, &root(), &v1, vec![no_op(&alice)], None)
        .await;
    assert_eq!(
        response.error_message.as_deref(),
        Some("Author field on delta must match logged in user")
    );
    // (The front end never saw the anonymous submission.)
    assert_eq!(server.metric("submit_errors_total").await, 3);
    server.shutdown().await;
}

// ---- Sign-out and socket authentication ----

#[tokio::test]
async fn sign_out_invalidates_the_socket() {
    let server = TestServer::start().await;
    let alice = user("alice");
    let cookie = server.user("alice").await;
    let other_cookie = server.user("alice").await;
    let mut ws = WaveClient::with_cookie(&server, &cookie).await;
    let mut other_ws = WaveClient::with_cookie(&server, &other_cookie).await;
    let v1 = create_wave(&mut ws, &[&alice]).await;
    ws.open(WAVE, Vec::new()).await;
    let session: Value = server
        .get("/session", Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    let socket_token = session["socket_token"].as_str().unwrap().to_owned();

    let response = server
        .http
        .post(server.url("/auth/signout"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["set-cookie"],
        "WSESSIONID=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"
    );
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"status": "SUCCESS"})
    );

    // The socket of that session is closed by the server,
    assert_eq!(ws.closed().await, Some((1008, "signed out".to_owned())));
    server.await_metric("open_subscriptions", 0).await;
    // its cookie and its socket token are worth nothing any more,
    let session: Value = server
        .get("/session", Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(session.get("address"), None);
    let mut stale = WaveClient::with_cookie(&server, &cookie).await;
    let seq = stale.send_open(WAVE, Vec::new()).await;
    assert_eq!(
        stale.next_for(seq).await.failure().as_deref(),
        Some("Not logged in")
    );
    assert_eq!(
        stale.authenticate(&socket_token).await.failure().as_deref(),
        Some("Auth token invalid")
    );
    // while the user's other session carries on.
    other_ws
        .submit_ok(&alice, &root(), &v1, vec![no_op(&alice)], None)
        .await;
    // Signing out again, or without a session, is fine (`SignOutServletTest`).
    for cookie in [Some(&cookie), None] {
        let mut request = server.http.post(server.url("/auth/signout"));
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        assert_eq!(request.send().await.unwrap().status(), 200);
    }
    server.shutdown().await;
}

#[tokio::test]
async fn sockets_authenticate_with_the_session_token() {
    let server = TestServer::start().await;
    let alice = user("alice");
    let alice_cookie = server.user("alice").await;
    let bob_cookie = server.user("bob").await;
    let token_of = |cookie: String| {
        let server = &server;
        async move {
            let session: Value = server
                .get("/session", Some(&cookie))
                .await
                .json()
                .await
                .unwrap();
            session["socket_token"].as_str().unwrap().to_owned()
        }
    };
    let alice_token = token_of(alice_cookie.clone()).await;
    let bob_token = token_of(bob_cookie.clone()).await;
    assert!(alice_token.starts_with("wn_ws_"));
    assert_ne!(alice_token, bob_token);
    // The token is not the cookie, and the same each time it is asked for.
    assert!(!alice_cookie.contains(alice_token.trim_start_matches("wn_ws_")));
    assert_eq!(token_of(alice_cookie.clone()).await, alice_token);

    // A connection without credentials, as a browser on another origin
    // policy or a native client would open it.
    let mut ws = WaveClient::connect(&server, &[]).await;
    let seq = ws.send_open(WAVE, Vec::new()).await;
    assert_eq!(
        ws.next_for(seq).await.failure().as_deref(),
        Some("Not logged in")
    );
    assert_eq!(
        ws.authenticate("wn_ws_bogus").await.failure().as_deref(),
        Some("Auth token invalid")
    );
    assert_eq!(
        ws.authenticate("").await.failure().as_deref(),
        Some("Auth token invalid")
    );
    let result = ws.authenticate(&alice_token).await;
    let _: ProtocolAuthenticationResult = result.decode();
    assert_eq!(
        result.text,
        format!(
            "{{\"messageType\":\"ProtocolAuthenticationResult\",\"sequenceNumber\":{},\
             \"message\":{{}}}}",
            result.seq
        )
    );
    create_wave(&mut ws, &[&alice]).await;
    let view = ws.open(WAVE, Vec::new()).await;
    assert!(view.initial[0].snapshot.is_some());
    // Authenticating again as the same user is fine; as another is not.
    let _: ProtocolAuthenticationResult = ws.authenticate(&alice_token).await.decode();
    assert_eq!(
        ws.authenticate(&bob_token).await.failure().as_deref(),
        Some("Session already authenticated as a different user")
    );
    // A cookie-authenticated socket cannot switch users either.
    let mut cookie_ws = WaveClient::with_cookie(&server, &alice_cookie).await;
    assert_eq!(
        cookie_ws
            .authenticate(&bob_token)
            .await
            .failure()
            .as_deref(),
        Some("Session already authenticated as a different user")
    );
    server.shutdown().await;
}

// ---- Bearer tokens ----

#[tokio::test]
async fn bearer_tokens_give_scoped_access() {
    let server = TestServer::start().await;
    let alice = user("alice");
    let cookie = server.user("alice").await;
    let mut ws = WaveClient::with_cookie(&server, &cookie).await;
    let v1 = create_wave(&mut ws, &[&alice]).await;

    // Token management needs the session.
    assert_eq!(server.get("/auth/tokens", None).await.status(), 401);
    let (read_token, read_id) = server.create_token(&cookie, "reader", &["read"]).await;
    let created = server
        .http
        .post(server.url("/auth/tokens"))
        .header("Cookie", &cookie)
        .json(&json!({"name": "agent", "expires_in_secs": 3600}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let created: Value = created.json().await.unwrap();
    let full_token = created["token"].as_str().unwrap().to_owned();
    assert!(full_token.starts_with("wn_pat_"));
    assert_eq!(created["scopes"], json!(["read", "write"]));
    assert_eq!(created["name"], "agent");
    assert!(created["expires_at"].as_i64().unwrap() > created["created_at"].as_i64().unwrap());

    // The list never shows the tokens themselves.
    let listed = server.get("/auth/tokens", Some(&cookie)).await;
    assert_eq!(listed.status(), 200);
    let listed = listed.text().await.unwrap();
    assert!(!listed.contains(&full_token) && !listed.contains(&read_token));
    let listed: Value = serde_json::from_str(&listed).unwrap();
    let tokens = listed["tokens"].as_array().unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0]["name"], "reader");
    assert_eq!(tokens[0]["scopes"], json!(["read"]));
    assert_eq!(tokens[0]["id"], read_id);
    assert_eq!(tokens[0]["expires_at"], Value::Null);
    assert_eq!(tokens[0].get("token"), None);
    for bad in [
        json!({"name": ""}),
        json!({"name": "x", "scopes": ["admin"]}),
        json!({"name": "x", "scopes": []}),
        json!({"name": "x", "expires_in_secs": 0}),
        json!({"name": "x", "surprise": true}),
        json!("nonsense"),
    ] {
        let response = server
            .http
            .post(server.url("/auth/tokens"))
            .header("Cookie", &cookie)
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{bad}");
    }
    // A token cannot manage tokens.
    assert_eq!(
        server
            .get_bearer("/auth/tokens", &full_token)
            .await
            .status(),
        403
    );
    let response = server
        .http
        .post(server.url("/auth/tokens"))
        .bearer_auth(&full_token)
        .json(&json!({"name": "more"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);

    // HTTP with a bearer token.
    let fetched = server
        .get_bearer("/fetch/example.com/w+abc/~/conv+root", &read_token)
        .await;
    assert_eq!(fetched.status(), 200);
    let session: Value = server
        .get_bearer("/session", &read_token)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(session["address"], "alice@example.com");
    assert_eq!(session.get("socket_token"), None);
    let invalid = server.get_bearer("/session", "wn_pat_invalid").await;
    assert_eq!(invalid.status(), 401);
    assert_eq!(
        invalid.headers()["www-authenticate"],
        "Bearer error=\"invalid_token\""
    );
    // An invalid token is not rescued by a valid cookie.
    let both = server
        .http
        .get(server.url("/session"))
        .bearer_auth("wn_pat_invalid")
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(both.status(), 401);
    // Writing needs the write scope.
    let set_locale = |token: String| {
        let server = &server;
        async move {
            server
                .http
                .post(server.url("/locale/?locale=en_GB"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(set_locale(read_token.clone()).await, 403);
    assert_eq!(set_locale(full_token.clone()).await, 200);

    // The WebSocket with a bearer token at the upgrade.
    let authorization = format!("Bearer {read_token}");
    let mut reader = WaveClient::connect(&server, &[("Authorization", &authorization)]).await;
    let view = reader.open(WAVE, Vec::new()).await;
    assert!(view.initial[0].snapshot.is_some());
    let response = reader
        .submit(&alice, &root(), &v1, vec![no_op(&alice)], None)
        .await;
    assert_eq!(response.operations_applied, Some(0));
    assert_eq!(
        response.error_message.as_deref(),
        Some("Token does not have the write scope")
    );
    // Or presented in the protocol, where a browser cannot set headers.
    let mut writer = WaveClient::connect(&server, &[]).await;
    let _: ProtocolAuthenticationResult = writer.authenticate(&full_token).await.decode();
    let v2 = writer
        .submit_ok(&alice, &root(), &v1, vec![no_op(&alice)], None)
        .await;
    assert_eq!(versions(&reader.next_update(&view).await), [1]);
    // A write-only token cannot open views.
    let (write_token, _) = server.create_token(&cookie, "writer", &["write"]).await;
    let mut write_only = WaveClient::connect(&server, &[]).await;
    let _: ProtocolAuthenticationResult = write_only.authenticate(&write_token).await.decode();
    let seq = write_only.send_open(WAVE, Vec::new()).await;
    assert_eq!(
        write_only.next_for(seq).await.failure().as_deref(),
        Some("Token does not have the read scope")
    );
    write_only
        .submit_ok(&alice, &root(), &v2, vec![no_op(&alice)], None)
        .await;
    assert_eq!(
        server
            .get_bearer("/fetch/example.com/w+abc/~/conv+root", &write_token)
            .await
            .status(),
        403
    );
    // A rejected bearer token at the upgrade refuses the upgrade.
    assert!(
        try_connect(&server, &[("Authorization", "Bearer wn_pat_invalid")])
            .await
            .is_err()
    );

    // Revoking a token closes the connections that use it.
    let revoked = server
        .http
        .delete(server.url(&format!("/auth/tokens/{read_id}")))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), 204);
    assert_eq!(reader.closed().await, Some((1008, "signed out".to_owned())));
    assert_eq!(
        server.get_bearer("/session", &read_token).await.status(),
        401
    );
    let again = server
        .http
        .delete(server.url(&format!("/auth/tokens/{read_id}")))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 404);
    // The other connections are untouched.
    writer
        .submit(&alice, &root(), &v2, vec![no_op(&alice)], None)
        .await;
    assert_eq!(
        server.get_bearer("/session", &full_token).await.status(),
        200
    );
    server.shutdown().await;
}

#[tokio::test]
async fn robots_use_bearer_tokens_like_everyone_else() {
    use resonator_server::account::{AccountData, RobotAccountData};
    let server = TestServer::start().await;
    let robot = user("helper");
    let auth = &server.app().state().auth;
    auth.accounts()
        .put_account(AccountData::Robot(RobotAccountData::agent(robot.clone())))
        .await
        .unwrap();
    let token = auth.issue_robot_token(&robot).await.unwrap();

    let authorization = format!("Bearer {token}");
    let mut ws = WaveClient::connect(&server, &[("Authorization", &authorization)]).await;
    let v = ws
        .submit_ok(
            &robot,
            &root(),
            &version_zero(&root()),
            vec![add_participant(&robot, &robot)],
            None,
        )
        .await;
    assert_eq!(v.version(), 1);
    let view = ws.open(WAVE, Vec::new()).await;
    assert!(view.initial[0].snapshot.is_some());
    let session: Value = server
        .get_bearer("/session", &token)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(session["address"], "helper@example.com");
    // A robot has no password to sign in with.
    assert_eq!(server.sign_in_response("helper", "").await.status(), 403);
    // A new token replaces the old one and closes its connections.
    let replacement = auth.issue_robot_token(&robot).await.unwrap();
    assert_eq!(ws.closed().await, Some((1008, "signed out".to_owned())));
    assert_eq!(server.get_bearer("/session", &token).await.status(), 401);
    assert_eq!(
        server.get_bearer("/session", &replacement).await.status(),
        200
    );
    server.shutdown().await;
}

// ---- Rate limiting and CSRF ----

#[tokio::test]
async fn sign_in_is_rate_limited() {
    let server = TestServer::start_with(|config| {
        config.login_max_failures = 3;
        config.login_backoff_base_secs = 30;
        config.register_max_per_hour = 2;
    })
    .await;
    assert_eq!(server.register("frodo", "password").await.status(), 200);
    assert_eq!(server.register("sam", "password").await.status(), 200);
    for _ in 0..3 {
        let response = server.sign_in_response("frodo", "wrong").await;
        assert_eq!(response.status(), 403);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({
                "status": "FAILED",
                "message": "The username or password you entered is incorrect.",
            })
        );
    }
    // Now even the right password is refused for a while.
    let limited = server.sign_in_response("frodo", "password").await;
    assert_eq!(limited.status(), 429);
    assert_eq!(limited.headers()["retry-after"], "30");
    assert!(session_cookie(&limited).is_none());
    let body: Value = limited.json().await.unwrap();
    assert_eq!(body["status"], "FAILED");
    assert_eq!(
        body["message"],
        "Too many attempts. Try again in 30 seconds."
    );
    // Other accounts are not affected.
    assert_eq!(
        server.sign_in_response("sam", "password").await.status(),
        200
    );

    // Registration has its own limit per client.
    let limited = server.register("pippin", "password").await;
    assert_eq!(limited.status(), 429);
    assert!(limited.headers().contains_key("retry-after"));
    assert_eq!(server.metric("rate_limited_total").await, 2);
    assert_eq!(server.metric("sign_in_failures_total").await, 3);
    assert_eq!(server.metric("sign_ins_total").await, 1);
    server.shutdown().await;
}

#[tokio::test]
async fn cross_site_requests_are_rejected() {
    let server = TestServer::start_with(|config| {
        config.allowed_origins = vec!["https://app.example.net".to_owned()];
    })
    .await;
    let cookie = server.user("alice").await;
    let post = |path: &'static str, headers: Vec<(&'static str, String)>| {
        let server = &server;
        let cookie = cookie.clone();
        async move {
            let mut request = server
                .http
                .post(server.url(path))
                .header("Cookie", cookie)
                .json(&json!({"address": "alice", "password": "alice-password", "name": "t"}));
            for (name, value) in headers {
                request = request.header(name, value);
            }
            request.send().await.unwrap()
        }
    };
    let signed_in = || async {
        let session: Value = server
            .get("/session", Some(&cookie))
            .await
            .json()
            .await
            .unwrap();
        session.get("address").is_some()
    };

    // A page on another site posting with the user's cookie.
    for headers in [
        vec![("Origin", "http://evil.example".to_owned())],
        vec![("Origin", "null".to_owned())],
        vec![("Referer", "http://evil.example/page?x=1".to_owned())],
        vec![("Sec-Fetch-Site", "cross-site".to_owned())],
        vec![("Sec-Fetch-Site", "same-site".to_owned())],
        // The right host on the wrong port is another origin.
        vec![(
            "Origin",
            format!("http://127.0.0.1:{}", server.addr.port() + 1),
        )],
        vec![("Origin", format!("http://{}.evil.example", server.addr))],
    ] {
        for path in [
            "/auth/signout",
            "/auth/tokens",
            "/auth/signin",
            "/auth/register",
        ] {
            let response = post(path, headers.clone()).await;
            assert_eq!(response.status(), 403, "{path} {headers:?}");
            assert!(session_cookie(&response).is_none());
            assert_eq!(
                response.json::<Value>().await.unwrap(),
                json!({"status": "FAILED", "message": "Cross-site request rejected"})
            );
        }
    }
    assert!(signed_in().await, "the session survived");
    assert_eq!(server.metric("csrf_rejections_total").await, 7 * 4);
    let tokens: Value = server
        .get("/auth/tokens", Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(tokens["tokens"], json!([]));

    // The server's own pages, and configured origins, may.
    for headers in [
        vec![("Origin", server.origin())],
        vec![("Referer", format!("{}/auth/signin", server.origin()))],
        vec![("Origin", "https://app.example.net".to_owned())],
        vec![("Sec-Fetch-Site", "same-origin".to_owned())],
        vec![],
    ] {
        assert_eq!(
            post("/locale/?locale=en", headers.clone()).await.status(),
            200,
            "{headers:?}"
        );
    }

    // The WebSocket handshake is held to the same rule: browsers send
    // cookies with it wherever the page came from.
    for origin in ["http://evil.example", "null"] {
        let refused = try_connect(&server, &[("Cookie", &cookie), ("Origin", origin)]).await;
        assert!(refused.is_err(), "{origin}");
        // Even without a cookie.
        assert!(try_connect(&server, &[("Origin", origin)]).await.is_err());
    }
    let origin = server.origin();
    for origin in [origin.as_str(), "https://app.example.net"] {
        let mut ws = WaveClient::connect(&server, &[("Cookie", &cookie), ("Origin", origin)]).await;
        ws.open(WAVE, Vec::new()).await;
    }

    // A bearer token is no ambient credential, so its requests are exempt.
    let (token, _) = server
        .create_token(&cookie, "cli", &["read", "write"])
        .await;
    let response = server
        .http
        .post(server.url("/locale/?locale=en"))
        .bearer_auth(&token)
        .header("Origin", "http://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    // Safe methods are not affected.
    let response = server
        .http
        .get(server.url("/session"))
        .header("Cookie", &cookie)
        .header("Origin", "http://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    // Finally the real thing works from the right origin.
    let response = post("/auth/signout", vec![("Origin", server.origin())]).await;
    assert_eq!(response.status(), 200);
    assert!(!signed_in().await);
    server.shutdown().await;
}

// ---- Limits ----

#[tokio::test]
async fn oversized_messages_close_the_connection() {
    let server = TestServer::start_with(|config| {
        config.websocket_max_message_size = 16 * 1024;
        config.http_max_body_size = 1024;
    })
    .await;
    let alice = user("alice");
    let cookie = server.user("alice").await;
    let mut ws = WaveClient::with_cookie(&server, &cookie).await;
    let v1 = create_wave(&mut ws, &[&alice]).await;
    // Within the limit.
    let v2 = ws
        .submit_ok(
            &alice,
            &root(),
            &v1,
            vec![insert(&alice, "b+1", 0, 0, &"x".repeat(8 * 1024))],
            None,
        )
        .await;
    // Over it: the server drops the connection without applying anything.
    let delta = common::delta(
        &alice,
        &v2,
        vec![insert(&alice, "b+1", 8 * 1024, 0, &"y".repeat(32 * 1024))],
    );
    let _ = ws
        .send(
            &resonator_model::proto::waveclient_rpc::ProtocolSubmitRequest {
                wavelet_name: Some(wire_name(&root())),
                delta: Some(delta),
                channel_id: None,
            },
        )
        .await;
    match ws.closed().await {
        // The close frame may be lost if the server resets first.
        None => {}
        Some((code, _)) => assert_eq!(code, 1009),
    }
    server.await_metric("websocket_connections", 0).await;

    // The server is fine and the wavelet untouched.
    let mut ws = WaveClient::with_cookie(&server, &cookie).await;
    let view = ws.open(WAVE, Vec::new()).await;
    assert_eq!(resulting_version(&view.wavelets()[0]), v2);

    // HTTP bodies are limited too.
    let response = server
        .http
        .post(server.url("/auth/signin"))
        .json(&json!({"address": "alice", "password": "p".repeat(4096)}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_consumers_are_disconnected_by_message_count() {
    slow_consumer_is_disconnected(|config| config.websocket_send_queue = 8).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_consumers_are_disconnected_by_queued_bytes() {
    // The default allows 1024 messages; these are large, so the byte
    // limit is what stops them.
    slow_consumer_is_disconnected(|config| config.websocket_send_queue_bytes = 2 * 1024 * 1024)
        .await;
}

async fn slow_consumer_is_disconnected(
    configure: impl FnOnce(&mut resonator_server::config::Config),
) {
    let server = TestServer::start_with(configure).await;
    let (alice, bob) = (user("alice"), user("bob"));
    let alice_cookie = server.user("alice").await;
    let bob_cookie = server.user("bob").await;
    let mut alice_ws = WaveClient::with_cookie(&server, &alice_cookie).await;
    let mut version = create_wave(&mut alice_ws, &[&alice, &bob]).await;

    // Bob opens the wave and then stops reading.
    let mut bob_ws = WaveClient::with_cookie(&server, &bob_cookie).await;
    let view = bob_ws.open(WAVE, Vec::new()).await;
    assert!(view.initial[0].snapshot.is_some());
    assert_eq!(server.metric("open_subscriptions").await, 1);

    // Alice writes far more than the socket buffers between the server
    // and Bob can hold: each delta is a new blip of 256 KiB.
    let text = "z".repeat(256 * 1024);
    let mut disconnected = false;
    for n in 0..400 {
        version = alice_ws
            .submit_ok(
                &alice,
                &root(),
                &version,
                vec![insert(&alice, &format!("b+{n}"), 0, 0, &text)],
                None,
            )
            .await;
        if n % 8 == 7 && server.metric("websocket_slow_consumers_total").await == 1 {
            disconnected = true;
            break;
        }
    }
    assert!(disconnected, "the slow client was never disconnected");
    // His subscription and connection are gone, so nothing accumulates
    // for him; Alice is unaffected.
    server.await_metric("open_subscriptions", 0).await;
    server.await_metric("websocket_connections", 1).await;
    alice_ws
        .submit_ok(&alice, &root(), &version, vec![no_op(&alice)], None)
        .await;
    // When Bob finally reads, he finds what was buffered and then the end
    // of the connection.
    let mut received = 0;
    while let Frame::Message(_) = bob_ws.next().await {
        received += 1;
    }
    assert!(received < 400);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_views_are_paced_not_dropped() {
    // Opening a wave whose snapshots exceed the queue's byte limit several
    // times over works for a client that reads: the snapshots are paced
    // by the connection instead of counted against it.
    let server = TestServer::start_with(|config| {
        config.websocket_send_queue_bytes = 1024 * 1024;
    })
    .await;
    let alice = user("alice");
    let cookie = server.user("alice").await;
    let mut ws = WaveClient::with_cookie(&server, &cookie).await;
    let text = "w".repeat(700 * 1024);
    let wavelets = ["conv+root", "conv+second", "conv+third"];
    for wavelet in wavelets {
        let name = wavelet_name("w+abc", wavelet);
        let mut version = ws
            .submit_ok(
                &alice,
                &name,
                &version_zero(&name),
                vec![add_participant(&alice, &alice)],
                None,
            )
            .await;
        for blip in ["b+1", "b+2", "b+3"] {
            version = ws
                .submit_ok(
                    &alice,
                    &name,
                    &version,
                    vec![insert(&alice, blip, 0, 0, &text)],
                    None,
                )
                .await;
        }
    }
    for _ in 0..3 {
        let view = ws.open(WAVE, Vec::new()).await;
        assert_eq!(view.wavelets().len(), 3);
        for update in view.wavelets() {
            let snapshot = update.snapshot.as_ref().expect("snapshot");
            assert_eq!(snapshot.document.len(), 3);
            assert_eq!(resulting_version(update).version(), 4);
        }
        ws.cancel(view.seq).await;
    }
    assert_eq!(server.metric("websocket_slow_consumers_total").await, 0);
    // The connection is alive and well.
    let name = wavelet_name("w+abc", "conv+root");
    ws.submit(
        &alice,
        &name,
        &HashedVersion::unsigned(4),
        vec![no_op(&alice)],
        None,
    )
    .await;
    server.shutdown().await;
}

#[tokio::test]
async fn idle_connections_are_pinged_and_dropped() {
    let server = TestServer::start_with(|config| {
        config.websocket_ping_interval_secs = 1;
        config.websocket_idle_timeout_secs = 2;
    })
    .await;
    let cookie = server.user("alice").await;
    // A client that keeps reading answers the pings and stays connected.
    let mut lively = WaveClient::with_cookie(&server, &cookie).await;
    let mut pings = 0;
    let listen_until = tokio::time::Instant::now() + Duration::from_millis(4500);
    while let Ok(frame) = tokio::time::timeout_at(listen_until, {
        use futures_util::StreamExt;
        lively.socket.next()
    })
    .await
    {
        match frame {
            Some(Ok(Message::Ping(_))) => {
                pings += 1;
                // Reading queues the pong; writing sends it.
                lively.socket.flush().await.unwrap();
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(pings >= 3, "{pings} pings");
    lively.open(WAVE, Vec::new()).await;

    drop(lively);

    // One that goes silent is dropped after the idle timeout.
    let mut silent = WaveClient::with_cookie(&server, &cookie).await;
    tokio::time::sleep(Duration::from_millis(4500)).await;
    assert_eq!(
        silent.closed().await,
        Some((1008, "idle timeout".to_owned()))
    );
    server.await_metric("websocket_connections", 0).await;
    server.shutdown().await;
}

#[tokio::test]
async fn shutdown_closes_the_sockets() {
    let server = TestServer::start().await;
    let alice = user("alice");
    let cookie = server.user("alice").await;
    let mut ws = WaveClient::with_cookie(&server, &cookie).await;
    create_wave(&mut ws, &[&alice]).await;
    ws.open(WAVE, Vec::new()).await;
    let addr = server.addr;
    let started = std::time::Instant::now();
    server.shutdown().await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        ws.closed().await,
        Some((1001, "server shutting down".to_owned()))
    );
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
}

// ---- Protocol errors ----

/// The error text of the failed `RpcFinished` that answers `seq`.
async fn failure_of(ws: &mut WaveClient, seq: i32) -> String {
    ws.next_for(seq)
        .await
        .failure()
        .expect("a failed RpcFinished")
}

#[tokio::test]
async fn protocol_errors_are_answered_not_fatal() {
    let server = TestServer::start_with(|config| {
        config.websocket_max_subscriptions = 2;
    })
    .await;
    let alice = user("alice");
    let cookie = server.user("alice").await;
    let mut ws = WaveClient::with_cookie(&server, &cookie).await;
    let v1 = create_wave(&mut ws, &[&alice]).await;
    // An unknown message type.
    ws.send_text(r#"{"messageType":"ProtocolBogus","sequenceNumber":900,"message":{}}"#)
        .await;
    assert_eq!(
        failure_of(&mut ws, 900).await,
        "Unknown proto class: ProtocolBogus"
    );
    // A body that does not fit its type, or lacks required fields.
    ws.send_text(r#"{"messageType":"ProtocolOpenRequest","sequenceNumber":901,"message":{"1":5}}"#)
        .await;
    assert_eq!(
        failure_of(&mut ws, 901).await,
        "Invalid message: field 1: expected a string"
    );
    ws.send_text(r#"{"messageType":"ProtocolSubmitRequest","sequenceNumber":902,"message":{}}"#)
        .await;
    assert_eq!(
        failure_of(&mut ws, 902).await,
        "Invalid message: message missing required field: ProtocolSubmitRequest.wavelet_name"
    );
    ws.send_text(r#"{"messageType":"ProtocolOpenRequest","sequenceNumber":903}"#)
        .await;
    assert!(
        failure_of(&mut ws, 903)
            .await
            .starts_with("Invalid message: ")
    );
    // Invalid ids.
    let seq = ws.send_open("not a wave id", Vec::new()).await;
    assert!(failure_of(&mut ws, seq).await.starts_with("Invalid id"));
    let seq = ws
        .send(
            &resonator_model::proto::waveclient_rpc::ProtocolSubmitRequest {
                wavelet_name: Some("nonsense".to_owned()),
                delta: Some(common::delta(&alice, &v1, vec![no_op(&alice)])),
                channel_id: None,
            },
        )
        .await;
    assert!(failure_of(&mut ws, seq).await.starts_with("Invalid id"));

    // An unpaired surrogate in a string (which JavaScript happily sends)
    // cannot be represented here: the request fails, the connection lives.
    let delta = frames::submit_text(
        904,
        &wire_name(&root()),
        &serialize_hashed_version(&v1).unwrap(),
        "\\ud83d",
    );
    ws.send_text(&delta).await;
    let error = failure_of(&mut ws, 904).await;
    assert!(
        error.starts_with("Invalid message: invalid JSON"),
        "{error}"
    );
    // The same frame with a proper pair is fine.
    let delta = frames::submit_text(
        905,
        &wire_name(&root()),
        &serialize_hashed_version(&v1).unwrap(),
        "\\ud83d\\ude00",
    );
    ws.send_text(&delta).await;
    let response: ProtocolSubmitResponse = ws.next_for(905).await.decode();
    assert_eq!(response.error_message, None);
    let v2 =
        deserialize_hashed_version(&response.hashed_version_after_application.unwrap()).unwrap();
    let fetched = server
        .get("/fetch/example.com/w+abc/~/conv+root/b+1", Some(&cookie))
        .await
        .text()
        .await
        .unwrap();
    assert!(fetched.contains("\u{1F600}"), "{fetched}");

    // Server errors of a submission come back in the response.
    let response = ws.submit(&alice, &root(), &v1, Vec::new(), None).await;
    assert_eq!(
        response.error_message.as_deref(),
        Some("Empty delta at version 1")
    );
    let response = ws
        .submit(
            &alice,
            &root(),
            &HashedVersion::of(2, vec![1; 20]),
            vec![no_op(&alice)],
            None,
        )
        .await;
    assert!(
        response
            .error_message
            .as_deref()
            .unwrap()
            .starts_with("Mismatched hash at version 2"),
        "{response:?}"
    );

    // Cancelling an open stream ends it in the standard manner.
    let view = ws.open(WAVE, Vec::new()).await;
    assert_eq!(server.metric("open_subscriptions").await, 1);
    ws.cancel(view.seq).await;
    loop {
        // Commit notices may still be on their way out.
        let message = ws.next_for(view.seq).await;
        if message.kind == "RpcFinished" {
            assert_eq!(message.decode::<RpcFinished>(), RpcFinished::success());
            assert_eq!(
                message.text,
                format!(
                    "{{\"messageType\":\"RpcFinished\",\"sequenceNumber\":{},\"message\":\
                     {{\"1\":false}}}}",
                    view.seq
                )
            );
            break;
        }
    }
    assert_eq!(server.metric("open_subscriptions").await, 0);
    let v3 = ws
        .submit_ok(&alice, &root(), &v2, vec![no_op(&alice)], None)
        .await;
    ws.expect_silence().await;
    // Cancelling what is not open is ignored.
    ws.cancel(view.seq).await;
    ws.cancel(12345).await;

    // A connection has a limit of open views.
    let first = ws.open(WAVE, Vec::new()).await;
    let _second = ws.open(WAVE, Vec::new()).await;
    let seq = ws.send_open(WAVE, Vec::new()).await;
    assert_eq!(
        failure_of(&mut ws, seq).await,
        "Too many open wave views on this connection"
    );
    ws.cancel(first.seq).await;
    ws.open(WAVE, Vec::new()).await;
    assert_eq!(resulting_version(&first.wavelets()[0]), v3);

    // Reusing the sequence number of an open stream is a protocol error.
    let mut clumsy = WaveClient::with_cookie(&server, &cookie).await;
    let view = clumsy.open(WAVE, Vec::new()).await;
    clumsy
        .send_as(
            view.seq,
            &ProtocolOpenRequest {
                participant_id: Some(String::new()),
                wave_id: Some(WAVE.to_owned()),
                wavelet_id_prefix: Vec::new(),
                known_wavelet: Vec::new(),
            },
        )
        .await;
    assert_eq!(clumsy.closed().await.map(|(code, _)| code), Some(1002));

    // Frames that are no envelope at all end the connection.
    for garbage in ["not json", "[1,2,3]", r#"{"messageType":"X"}"#, ""] {
        let mut ws = WaveClient::with_cookie(&server, &cookie).await;
        ws.send_text(garbage).await;
        assert_eq!(
            ws.closed().await,
            Some((1007, "not a JSON envelope".to_owned())),
            "{garbage:?}"
        );
    }
    let mut binary = WaveClient::with_cookie(&server, &cookie).await;
    binary
        .socket
        .send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();
    assert_eq!(
        binary.closed().await,
        Some((1003, "text frames only".to_owned()))
    );
    // The first connection survived all of that.
    ws.submit_ok(&alice, &root(), &v3, vec![no_op(&alice)], None)
        .await;
    server.shutdown().await;
}

/// Hand-written frames for the tests that need what the typed API cannot
/// produce.
mod frames {
    use resonator_model::proto::federation::ProtocolHashedVersion;
    use resonator_model::proto::json::to_json;

    /// A submit request inserting `escaped_text` (already JSON-escaped)
    /// into blip `b+1` of an empty document, by alice.
    pub fn submit_text(
        seq: i32,
        wavelet_name: &str,
        version: &ProtocolHashedVersion,
        escaped_text: &str,
    ) -> String {
        let version = to_json(version).unwrap();
        format!(
            "{{\"messageType\":\"ProtocolSubmitRequest\",\"sequenceNumber\":{seq},\"message\":\
             {{\"1\":\"{wavelet_name}\",\"2\":{{\"1\":{version},\"2\":\"alice@example.com\",\
             \"3\":[{{\"3\":{{\"1\":\"b+1\",\"2\":{{\"1\":[{{\"2\":\"{escaped_text}\"}}]}}}}}}],\
             \"4\":[]}}}}}}"
        )
    }
}

// ---- The HTTP endpoints ----

#[tokio::test]
async fn the_session_names_other_servers_to_register_on() {
    let server = TestServer::start().await;
    let session: Value = server.get("/session", None).await.json().await.unwrap();
    assert_eq!(session.get("other_servers"), None);
    server.shutdown().await;

    let server = TestServer::start_with(|config| {
        config.other_servers = vec!["other.example".to_owned()];
    })
    .await;
    let session: Value = server.get("/session", None).await.json().await.unwrap();
    assert_eq!(session["other_servers"], json!(["other.example"]));
    server.shutdown().await;

    // Not while registration is closed here.
    let server = TestServer::start_with(|config| {
        config.other_servers = vec!["other.example".to_owned()];
        config.registration_enabled = false;
    })
    .await;
    let session: Value = server.get("/session", None).await.json().await.unwrap();
    assert_eq!(session.get("other_servers"), None);
    server.shutdown().await;
}

#[tokio::test]
async fn registration_follows_the_original_rules() {
    let server = TestServer::start().await;
    let ok = server.register("  Frodo ", "password").await;
    assert_eq!(ok.status(), 200);
    assert_eq!(ok.headers()["content-type"], "application/json");
    assert_eq!(
        ok.json::<Value>().await.unwrap(),
        json!({
            "status": "SUCCESS",
            "message": "Registration complete.",
            "address": "frodo@example.com",
        })
    );
    let refused = |name: &'static str, password: &'static str| {
        let server = &server;
        async move {
            let response = server.register(name, password).await;
            assert_eq!(response.status(), 403, "{name}");
            let body: Value = response.json().await.unwrap();
            assert_eq!(body["status"], "FAILED");
            body["message"].as_str().unwrap().to_owned()
        }
    };
    assert_eq!(refused("frodo", "password").await, "Account already exists");
    assert_eq!(
        refused("frodo@example.com", "other-password").await,
        "Account already exists"
    );
    assert_eq!(
        refused("bilbo@example2.com", "password").await,
        "Invalid address 'bilbo@example2.com': You can only create users at the example.com domain"
    );
    assert_eq!(
        refused("fro do", "password").await,
        "Invalid address 'fro do': Only letters (a-z), numbers (0-9), and periods (.) are \
         allowed in Username"
    );
    assert_eq!(
        refused("", "password").await,
        "Invalid address '': Username portion of address cannot be empty"
    );
    assert_eq!(
        refused("sam", "short").await,
        "Password must be at least 8 characters long"
    );
    // A request without the fields at all.
    let response = server
        .http
        .post(server.url("/auth/register"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    let response = server
        .http
        .post(server.url("/auth/register"))
        .header("Content-Type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);

    // A plain HTML form works too.
    let form = server
        .http
        .post(server.url("/auth/register"))
        .header("Accept", "text/html,application/xhtml+xml")
        .form(&[("address", "sam"), ("password", "samwise-gamgee")])
        .send()
        .await
        .unwrap();
    assert_eq!(form.status(), 200);
    assert_eq!(form.headers()["content-type"], "text/html;charset=utf-8");
    assert!(
        form.text()
            .await
            .unwrap()
            .contains("Registration complete.")
    );
    let form = server
        .http
        .post(server.url("/auth/register"))
        .header("Accept", "text/html")
        .form(&[("address", "sam"), ("password", "samwise-gamgee")])
        .send()
        .await
        .unwrap();
    assert_eq!(form.status(), 403);
    assert!(
        form.text()
            .await
            .unwrap()
            .contains("Account already exists")
    );
    let page = server.get("/auth/register", None).await;
    assert_eq!(page.status(), 200);
    assert!(page.text().await.unwrap().contains("<form method=\"post\""));
    // The original password still works; the refused ones changed nothing.
    assert_eq!(
        server.sign_in_response("frodo", "password").await.status(),
        200
    );
    assert_eq!(
        server
            .sign_in_response("frodo", "other-password")
            .await
            .status(),
        403
    );
    server.shutdown().await;

    // With registration disabled only the admin registers.
    let server = TestServer::start_with(|config| {
        config.registration_enabled = false;
        config.admin_user = "admin@example.com".to_owned();
    })
    .await;
    let response = server.register("frodo", "password").await;
    assert_eq!(response.status(), 403);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"status": "FAILED", "message": "Registration disabled by administrator"})
    );
    let page = server
        .get("/auth/register", None)
        .await
        .text()
        .await
        .unwrap();
    assert!(page.contains("Registration disabled") && !page.contains("<form"));
    let session: Value = server.get("/session", None).await.json().await.unwrap();
    assert_eq!(session["registration_enabled"], false);
    // (The admin account itself is created out of band.)
    let auth = &server.app().state().auth;
    let admin = resonator_server::auth::Identity::internal(user("admin"));
    auth.register(
        Some("admin"),
        "admin-password",
        "127.0.0.1".parse().unwrap(),
        Some(&admin),
    )
    .await
    .unwrap();
    let admin_cookie =
        session_cookie(&server.sign_in_response("admin", "admin-password").await).unwrap();
    let response = server
        .http
        .post(server.url("/auth/register"))
        .header("Cookie", &admin_cookie)
        .json(&json!({"address": "frodo", "password": "password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        server.sign_in_response("frodo", "password").await.status(),
        200
    );
    server.shutdown().await;
}

#[tokio::test]
async fn sign_in_sets_a_session_cookie() {
    let server = TestServer::start().await;
    server.register("frodo", "password").await;

    // `testGetReturnsSomething`.
    let page = server.get("/auth/signin", None).await;
    assert_eq!(page.status(), 200);
    assert!(page.text().await.unwrap().contains("name=\"password\""));

    // `testValidLoginWorks`, `testUserWithNoDomainGetsDomainAutomaticallyAdded`.
    for name in ["frodo@example.com", "frodo", "FRODO"] {
        let response = server.sign_in_response(name, "password").await;
        assert_eq!(response.status(), 200, "{name}");
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .to_owned();
        let (pair, attributes) = cookie.split_once("; ").unwrap();
        assert!(pair.starts_with("WSESSIONID=") && pair.len() == "WSESSIONID=".len() + 43);
        assert_eq!(
            attributes,
            "Path=/; HttpOnly; SameSite=Lax; Max-Age=1209600"
        );
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"status": "SUCCESS", "address": "frodo@example.com", "domain": "example.com"})
        );
        let session: Value = server
            .get("/session", Some(pair))
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(session["address"], "frodo@example.com");
    }

    // `testIncorrectPasswordReturns403`, `testInvalidUsernameReturns403`.
    for (name, password) in [
        ("frodo", "incorrect"),
        ("madeup@example.com", "incorrect"),
        ("@example.com", "password"),
        ("", ""),
    ] {
        let response = server.sign_in_response(name, password).await;
        assert_eq!(response.status(), 403, "{name}");
        assert!(session_cookie(&response).is_none());
    }
    let response = server
        .http
        .post(server.url("/auth/signin"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);

    // A form post redirects: `testLoginRedirects`,
    // `testLoginDoesNotRedirectToRemoteSite`.
    let form_sign_in = |query: &'static str, password: &'static str| {
        let server = &server;
        async move {
            server
                .http
                .post(server.url(&format!("/auth/signin{query}")))
                .header("Accept", "text/html")
                .form(&[("address", "frodo@example.com"), ("password", password)])
                .send()
                .await
                .unwrap()
        }
    };
    let response = form_sign_in("", "password").await;
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/");
    let cookie = session_cookie(&response).expect("cookie on the redirect");
    let response = form_sign_in("?r=/some/redir%20url", "password").await;
    assert_eq!(response.headers()["location"], "/some/redir url");
    for remote in [
        "?r=http://example.com/other/site",
        "?r=//example.com/",
        "?r=/%5Cexample.com",
        "?r=relative",
    ] {
        let response = form_sign_in(remote, "password").await;
        assert_eq!(response.status(), 303, "{remote}");
        assert_eq!(response.headers()["location"], "/", "{remote}");
    }
    let response = form_sign_in("?r=/x", "wrong").await;
    assert_eq!(response.status(), 403);
    let page = response.text().await.unwrap();
    assert!(page.contains("The username or password you entered is incorrect."));
    assert!(page.contains("action=\"/auth/signin?r=/x\""));

    // `testGetRedirects`: someone signed in is sent on.
    let response = server.get("/auth/signin", Some(&cookie)).await;
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/");
    let response = server
        .get("/auth/signin?r=/%23w%2Babc", Some(&cookie))
        .await;
    assert_eq!(response.headers()["location"], "/#w+abc");

    // Sign-out by form (`testUserSignedOut`, `testServletRedirects`,
    // `testServletDoesNotRedirectToRemoteUrl`).
    let page = server.get("/auth/signout", Some(&cookie)).await;
    assert_eq!(page.status(), 200);
    assert!(page.text().await.unwrap().contains("<form method=\"post\""));
    let session: Value = server
        .get("/session", Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        session["address"], "frodo@example.com",
        "GET does not sign out"
    );
    let sign_out = |query: &'static str, cookie: String| {
        let server = &server;
        async move {
            server
                .http
                .post(server.url(&format!("/auth/signout{query}")))
                .header("Accept", "text/html")
                .header("Cookie", cookie)
                .send()
                .await
                .unwrap()
        }
    };
    let response = sign_out("?r=/some/redir%20url", cookie.clone()).await;
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/some/redir url");
    let session: Value = server
        .get("/session", Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(session.get("address"), None);
    let response = sign_out("?r=http://example.com/other/site", cookie.clone()).await;
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("Logged out."));
    server.shutdown().await;
}

#[tokio::test]
async fn cookies_are_secure_behind_https() {
    // Told so by a trusted proxy.
    let server = TestServer::start_with(|config| {
        config.trusted_proxies = vec!["127.0.0.0/8".to_owned()];
    })
    .await;
    server.register("frodo", "password").await;
    let sign_in = |server: &TestServer, proto: Option<&'static str>| {
        let mut request = server
            .http
            .post(server.url("/auth/signin"))
            .json(&json!({"address": "frodo", "password": "password"}));
        if let Some(proto) = proto {
            request = request
                .header("X-Forwarded-Proto", proto)
                .header("X-Forwarded-For", "203.0.113.9");
        }
        async move { request.send().await.unwrap() }
    };
    let response = sign_in(&server, Some("https")).await;
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.ends_with("; Secure"), "{cookie}");
    assert_eq!(
        response.headers()["strict-transport-security"],
        "max-age=31536000"
    );
    let response = sign_in(&server, Some("http")).await;
    assert!(
        !response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Secure")
    );
    assert!(!response.headers().contains_key("strict-transport-security"));
    // The proxy's word is also taken for the client address: failures
    // from one forwarded address do not lock out another.
    server.shutdown().await;

    // From an untrusted peer the header means nothing.
    let server = TestServer::start().await;
    server.register("frodo", "password").await;
    let response = sign_in(&server, Some("https")).await;
    assert!(
        !response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Secure")
    );
    server.shutdown().await;

    // Or always, by configuration.
    let server = TestServer::start_with(|config| {
        config.cookie_secure = resonator_server::config::CookieSecure::Always;
    })
    .await;
    server.register("frodo", "password").await;
    let response = sign_in(&server, None).await;
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .ends_with("; Secure")
    );
    server.shutdown().await;
}

#[tokio::test]
async fn rate_limits_follow_the_forwarded_client_address() {
    let server = TestServer::start_with(|config| {
        config.trusted_proxies = vec!["127.0.0.1".to_owned()];
        config.login_max_failures = 2;
        config.login_backoff_base_secs = 60;
    })
    .await;
    server.register("frodo", "password").await;
    let sign_in = |forwarded: &'static str, password: &'static str| {
        let server = &server;
        async move {
            server
                .http
                .post(server.url("/auth/signin"))
                .header("X-Forwarded-For", forwarded)
                .json(&json!({"address": "frodo", "password": password}))
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(sign_in("203.0.113.9", "wrong").await, 403);
    assert_eq!(sign_in("198.51.100.1, 203.0.113.9", "wrong").await, 403);
    assert_eq!(sign_in("203.0.113.9", "password").await, 429);
    // Another client behind the same proxy is not held up, and a client
    // cannot dodge by prepending addresses of its own.
    assert_eq!(sign_in("203.0.113.10", "password").await, 200);
    assert_eq!(sign_in("10.9.9.9, 203.0.113.9", "password").await, 429);
    server.shutdown().await;
}

#[tokio::test]
async fn fetch_serves_snapshots_in_the_protocol_json() {
    let server = TestServer::start().await;
    let alice = user("alice");
    let cookie = server.user("alice").await;
    let mut ws = WaveClient::with_cookie(&server, &cookie).await;
    let v1 = create_wave(&mut ws, &[&alice]).await;
    let v2 = ws
        .submit_ok(
            &alice,
            &root(),
            &v1,
            vec![insert(&alice, "b+1", 0, 0, "hi")],
            None,
        )
        .await;

    // `testGetInvalidWaverefReturnsNotFound`.
    for path in [
        "/fetch/invalidwaveref",
        "/fetch/a/b/c",
        "/fetch/example.com/w+abc/~",
    ] {
        assert_eq!(
            server.get(path, Some(&cookie)).await.status(),
            404,
            "{path}"
        );
    }
    // `testGetMissingDataReturnsForbidden`.
    for path in [
        "/fetch/example.com/w+nosuchwave",
        "/fetch/example.com/w+abc/~/conv+nosuchwavelet",
        "/fetch/example.com/w+abc/~/conv+root/b+nosuchdoc",
        "/fetch/other.com/w+abc",
    ] {
        assert_eq!(
            server.get(path, Some(&cookie)).await.status(),
            403,
            "{path}"
        );
    }

    // `testGetWavelet`.
    let response = server
        .get("/fetch/example.com/w+abc/~/conv+root", Some(&cookie))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_eq!(response.headers()["cache-control"], "no-store");
    let text = response.text().await.unwrap();
    let wavelet: WaveletSnapshot = from_json(&text).unwrap();
    assert_eq!(wavelet.wavelet_id.as_deref(), Some("example.com/conv+root"));
    assert_eq!(wavelet.participant_id, ["alice@example.com"]);
    assert_eq!(
        wavelet.version,
        Some(serialize_hashed_version(&v2).unwrap())
    );
    assert_eq!(wavelet.document.len(), 1);
    // The exact shape: field numbers as keys, [low, high] longs, the
    // version number as a double, the hash in upper-case hex.
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["1"], "example.com/conv+root");
    assert_eq!(value["2"], json!(["alice@example.com"]));
    assert_eq!(value["4"]["1"], json!(2.0));
    assert!(text.contains("\"4\":{\"1\":2.0,\"2\":\""));
    let hash = value["4"]["2"].as_str().unwrap();
    assert_eq!(hash.len(), 40);
    assert!(
        hash.chars()
            .all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c))
    );
    assert!(value["5"].as_array().is_some_and(|long| long.len() == 2));
    assert_eq!(value["6"], "alice@example.com");
    assert_eq!(
        value["3"][0]["2"],
        json!({"1": [{"2": "hi"}]}),
        "the document as an operation"
    );
    assert_eq!(value["3"][0]["5"], json!([2, 0]));

    // `testGetDocument`.
    let text = server
        .get("/fetch/example.com/w+abc/~/conv+root/b+1", Some(&cookie))
        .await
        .text()
        .await
        .unwrap();
    let document: DocumentSnapshot = from_json(&text).unwrap();
    assert_eq!(document, wavelet.document[0]);
    assert!(text.starts_with("{\"1\":\"b+1\",\"2\":{\"1\":[{\"2\":\"hi\"}]},\"3\":\"alice@example.com\",\"4\":[\"alice@example.com\"],\"5\":[2,0],\"6\":["));

    // The wave: its root conversation wavelet in a view snapshot.
    let text = server
        .get("/fetch/example.com/w+abc", Some(&cookie))
        .await
        .text()
        .await
        .unwrap();
    let view: WaveViewSnapshot = from_json(&text).unwrap();
    assert_eq!(view.wave_id.as_deref(), Some("example.com/w+abc"));
    assert_eq!(view.wavelet, [wavelet]);
    // An explicit wavelet domain works as well as "~".
    assert_eq!(
        server
            .get(
                "/fetch/example.com/w+abc/example.com/conv+root",
                Some(&cookie)
            )
            .await
            .status(),
        200
    );
    server.shutdown().await;
}

#[tokio::test]
async fn the_other_endpoints() {
    let server = TestServer::start_with(|config| {
        config.public_address = "wave.example.com:443".to_owned();
    })
    .await;
    let cookie = server.user("john.smith").await;

    // The placeholder page and the security headers.
    let index = server.get("/", None).await;
    assert_eq!(index.status(), 200);
    let headers = index.headers().clone();
    assert_eq!(headers["content-type"], "text/html;charset=utf-8");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["x-frame-options"], "DENY");
    assert_eq!(headers["referrer-policy"], "same-origin");
    assert_eq!(headers["cross-origin-opener-policy"], "same-origin");
    assert_eq!(
        headers["content-security-policy"],
        "default-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'"
    );
    assert_eq!(headers["cache-control"], "no-store");
    assert!(!headers.contains_key("strict-transport-security"));
    let page = index.text().await.unwrap();
    assert!(page.contains("The wave server for example.com is running."));
    assert!(page.contains("/auth/signin"));
    let page = server.get("/", Some(&cookie)).await.text().await.unwrap();
    assert!(page.contains("Signed in as john.smith@example.com."));
    assert_eq!(server.get("/no/such/page", None).await.status(), 404);
    assert_eq!(
        server.get("/no/such/page", None).await.headers()["x-content-type-options"],
        "nosniff"
    );

    // Health.
    let health = server.get("/healthz", None).await;
    assert_eq!(health.status(), 200);
    assert_eq!(
        health.json::<Value>().await.unwrap(),
        json!({"status": "ok", "schema_version": resonator_server::storage::SCHEMA_VERSION})
    );

    // What the client needs to start.
    let session: Value = server.get("/session", None).await.json().await.unwrap();
    assert_eq!(session["domain"], "example.com");
    assert_eq!(session["websocket_address"], "wave.example.com:443");
    assert_eq!(session["registration_enabled"], true);
    assert_eq!(session.get("address"), None);
    assert_eq!(session.get("socket_token"), None);
    let seed = session["id"].as_str().unwrap();
    assert_eq!(seed.len(), 10);
    assert!(
        seed.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    );
    let signed_in: Value = server
        .get("/session", Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(signed_in["address"], "john.smith@example.com");
    assert!(
        signed_in["socket_token"]
            .as_str()
            .unwrap()
            .starts_with("wn_ws_")
    );
    assert_ne!(signed_in["id"], session["id"], "a fresh seed each time");

    // Profiles: 403 unless signed in; names and avatars from the address.
    assert_eq!(
        server
            .get("/profile/?addresses=a@example.com", None)
            .await
            .status(),
        403
    );
    assert_eq!(server.get("/profile/", Some(&cookie)).await.status(), 400);
    let profiles = server
        .get(
            "/profile/?addresses=john.smith%40example.com,sam_gamgee@example.com,%40example.com",
            Some(&cookie),
        )
        .await;
    assert_eq!(profiles.status(), 200);
    assert_eq!(profiles.headers()["content-type"], "application/json");
    assert_eq!(profiles.headers()["cache-control"], "no-store");
    assert_eq!(
        profiles.text().await.unwrap(),
        "{\"1\":[\
         {\"1\":\"john.smith@example.com\",\"2\":\"John Smith\",\
         \"3\":\"/iniavatars/100x100/john.smith@example.com\",\"4\":\"\"},\
         {\"1\":\"sam_gamgee@example.com\",\"2\":\"Sam Gamgee\",\
         \"3\":\"/iniavatars/100x100/sam_gamgee@example.com\",\"4\":\"\"},\
         {\"1\":\"@example.com\",\"2\":\"@example.com\",\
         \"3\":\"/iniavatars/100x100/@example.com\",\"4\":\"\"}]}"
    );
    assert_eq!(
        server
            .get("/profile?addresses=a@example.com", Some(&cookie))
            .await
            .status(),
        200
    );

    // Initials avatars.
    let avatar = server
        .get("/iniavatars/100x100/john.smith@example.com", None)
        .await;
    assert_eq!(avatar.status(), 200);
    assert_eq!(avatar.headers()["content-type"], "image/svg+xml");
    assert_eq!(avatar.headers()["cache-control"], "public, max-age=86400");
    let svg = avatar.text().await.unwrap();
    assert!(
        svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"100\"")
    );
    assert!(svg.contains(">JS</text>"));
    let hostile = server
        .get(
            "/iniavatars/9999x1/%3Cscript%3Ealert(1)%3C%2Fscript%3E@x",
            None,
        )
        .await
        .text()
        .await
        .unwrap();
    assert!(!hostile.contains("<script") && hostile.contains("width=\"1024\" height=\"8\""));

    // Locale.
    assert_eq!(server.get("/locale/", None).await.status(), 403);
    let locale: Value = server
        .get("/locale/", Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(locale, json!({"locale": null}));
    let set = |body: Option<&'static str>, query: &'static str, cookie: Option<String>| {
        let server = &server;
        async move {
            let mut request = server.http.post(server.url(&format!("/locale/{query}")));
            if let Some(cookie) = cookie {
                request = request.header("Cookie", cookie);
            }
            if let Some(body) = body {
                request = request
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(body);
            }
            request.send().await.unwrap().status()
        }
    };
    assert_eq!(set(None, "?locale=en", None).await, 403);
    assert_eq!(set(None, "?locale=pt_BR", Some(cookie.clone())).await, 200);
    let locale: Value = server
        .get("/locale/", Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(locale, json!({"locale": "pt_BR"}));
    assert_eq!(set(Some("locale=de"), "", Some(cookie.clone())).await, 200);
    let locale: Value = server
        .get("/locale", Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(locale, json!({"locale": "de"}));
    assert_eq!(set(None, "", Some(cookie.clone())).await, 400);
    assert_eq!(
        set(None, "?locale=%3Cscript%3E", Some(cookie.clone())).await,
        400
    );
    // Changing the locale keeps the account, its password and its session.
    let session: Value = server
        .get("/session", Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(session["address"], "john.smith@example.com");
    assert_eq!(
        server
            .sign_in_response("john.smith", "john.smith-password")
            .await
            .status(),
        200
    );

    // Wave references redirect into the client.
    let response = server
        .get("/waveref/example.com/w+abc/~/conv+root/b+1", Some(&cookie))
        .await;
    assert_eq!(response.status(), 302);
    assert_eq!(
        response.headers()["location"],
        "/#example.com/w+abc/~/conv+root/b+1"
    );
    let response = server.get("/waveref/example.com/w+abc", None).await;
    assert_eq!(response.status(), 302);
    assert_eq!(
        response.headers()["location"],
        "/auth/signin?r=/#example.com/w+abc"
    );

    // Metrics in the Prometheus text format.
    let metrics = server.get("/metrics", None).await;
    assert_eq!(metrics.status(), 200);
    assert_eq!(
        metrics.headers()["content-type"],
        "text/plain; version=0.0.4; charset=utf-8"
    );
    let text = metrics.text().await.unwrap();
    for name in [
        "websocket_connections",
        "open_subscriptions",
        "submits_total",
        "submit_errors_total",
        "db_writer_queue_depth",
        "http_requests_total",
    ] {
        assert!(
            text.contains(&format!("# TYPE resonator_{name} ")),
            "{name} in {text}"
        );
    }
    assert_eq!(server.metric("sign_ins_total").await, 2);
    server.shutdown().await;
}

#[tokio::test]
async fn metrics_can_be_protected_or_switched_off() {
    let server = TestServer::start_with(|config| {
        config.metrics_token = "s3cret-metrics-token".to_owned();
    })
    .await;
    let unauthorized = server.get("/metrics", None).await;
    assert_eq!(unauthorized.status(), 401);
    assert_eq!(unauthorized.headers()["www-authenticate"], "Bearer");
    assert_eq!(server.get_bearer("/metrics", "wrong").await.status(), 401);
    assert_eq!(
        server
            .get_bearer("/metrics", "s3cret-metrics-token-and-more")
            .await
            .status(),
        401
    );
    let authorized = server.get_bearer("/metrics", "s3cret-metrics-token").await;
    assert_eq!(authorized.status(), 200);
    assert!(
        authorized
            .text()
            .await
            .unwrap()
            .contains("resonator_http_requests_total")
    );
    // The metrics token is no access token.
    assert_eq!(
        server
            .get_bearer("/session", "s3cret-metrics-token")
            .await
            .status(),
        401
    );
    server.shutdown().await;

    let server = TestServer::start_with(|config| config.metrics_enabled = false).await;
    assert_eq!(server.get("/metrics", None).await.status(), 404);
    assert_eq!(server.get("/healthz", None).await.status(), 200);
    server.shutdown().await;
}

#[tokio::test]
async fn sessions_survive_a_restart() {
    // Sessions are in the database, not in memory.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("persistent.db");
    let start = || {
        let path = path.clone();
        TestServer::start_with(move |config| config.database_path = path)
    };
    let server = start().await;
    let alice = user("alice");
    let cookie = server.user("alice").await;
    let mut ws = WaveClient::with_cookie(&server, &cookie).await;
    let v1 = create_wave(&mut ws, &[&alice]).await;
    let (token, _) = server.create_token(&cookie, "t", &["read"]).await;
    server.shutdown().await;

    let server = start().await;
    let session: Value = server
        .get("/session", Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(session["address"], "alice@example.com");
    assert_eq!(server.get_bearer("/session", &token).await.status(), 200);
    let mut ws = WaveClient::with_cookie(&server, &cookie).await;
    let view = ws.open(WAVE, Vec::new()).await;
    assert_eq!(resulting_version(&view.wavelets()[0]), v1);
    // The reopened view resumes from the version known before the restart.
    let view = ws.open(WAVE, known("conv+root", &v1)).await;
    assert!(view.initial[0].snapshot.is_none());
    assert_eq!(resulting_version(&view.initial[0]), v1);
    server.shutdown().await;
}

#[tokio::test]
async fn directory_lists_and_filters_accounts_for_signed_in_users() {
    let server = TestServer::start().await;
    let cookie = server.user("alice").await;
    server.user("bob.builder").await;
    server.user("carol_100").await;

    // Not for anonymous callers.
    assert_eq!(server.get("/directory", None).await.status(), 403);

    let all: serde_json::Value = server
        .get("/directory", Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    let addresses: Vec<&str> = all["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|account| account["address"].as_str().unwrap())
        .collect();
    assert_eq!(
        addresses,
        [
            "alice@example.com",
            "bob.builder@example.com",
            "carol_100@example.com"
        ]
    );
    assert_eq!(all["accounts"][1]["name"], "Bob Builder");
    assert_eq!(all["accounts"][1]["kind"], "human");
    assert_eq!(
        all["accounts"][1]["image_url"],
        "/iniavatars/100x100/bob.builder@example.com"
    );
    assert!(!all.to_string().contains("password"));

    // Substring match, case-insensitive; LIKE wildcards are literal.
    for (query, expected) in [
        ("BOB", vec!["bob.builder@example.com"]),
        ("builder", vec!["bob.builder@example.com"]),
        ("_100", vec!["carol_100@example.com"]),
        ("b_b", vec![]),
        ("%25", vec![]),
        ("nobody", vec![]),
    ] {
        let found: serde_json::Value = server
            .get(&format!("/directory?query={query}"), Some(&cookie))
            .await
            .json()
            .await
            .unwrap();
        let got: Vec<&str> = found["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|account| account["address"].as_str().unwrap())
            .collect();
        assert_eq!(got, expected, "query {query}");
    }
    let limited: serde_json::Value = server
        .get("/directory?limit=2", Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(limited["accounts"].as_array().unwrap().len(), 2);
    server.shutdown().await;
}

#[tokio::test]
async fn directory_can_be_switched_off() {
    let server = TestServer::start_with(|config| config.directory_enabled = false).await;
    let cookie = server.user("alice").await;
    assert_eq!(server.get("/directory", Some(&cookie)).await.status(), 404);
    server.shutdown().await;
}
