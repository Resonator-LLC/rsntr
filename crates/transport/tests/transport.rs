//! Offline (localhost, relay-disabled) integration tests for the iroh
//! transport, plus one #[ignore]d relay-path test run manually for M2
//! acceptance.

use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use resonator_protocol::{
    ALPN, Done, EnvelopeObject, ErrorCode, Hello, ResultHeader, Row, Statement, Value,
    decode_envelope, encode_envelope,
};
use resonator_transport::{
    IncomingRequest, IrohConfig, IrohRequestStream, IrohTransport, PLAINTEXT_BANNER, RequestStream,
    Transport, TransportError, basic_hello, endpoint_id_from_secret, mint_ticket, parse_ticket,
};
use tokio::sync::mpsc;

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

type Incoming = mpsc::Receiver<IncomingRequest<IrohRequestStream>>;

fn server_hello() -> Hello {
    basic_hello(
        &["help", "sql-sqlite-3.46"],
        Some("resonator test node; query mod 'help' for usage"),
    )
}

fn client_hello() -> Hello {
    basic_hello(&["help"], None)
}

fn query(id: &str, modulation: &str, signal: &str) -> EnvelopeObject {
    EnvelopeObject::Query(Statement {
        id: id.to_string(),
        modulation: modulation.to_string(),
        signal: signal.to_string(),
        params: Vec::new(),
        database: None,
        row_limit: None,
        byte_limit: None,
        timeout_ms: None,
    })
}

/// Two offline transports wired together through the manual address book.
async fn offline_pair() -> (Arc<IrohTransport>, Arc<IrohTransport>, Incoming) {
    let (server, server_rx) = IrohTransport::bind(IrohConfig::offline(server_hello()))
        .await
        .expect("bind server");
    let (client, _client_rx) = IrohTransport::bind(IrohConfig::offline(client_hello()))
        .await
        .expect("bind client");
    client.add_peer_addrs(server.peer_id(), server.direct_addrs());
    (server, client, server_rx)
}

/// Answers one incoming request with a one-row result; returns the receiver
/// and the request that was served (its stream already finished).
async fn serve_one(mut rx: Incoming) -> (Incoming, IncomingRequest<IrohRequestStream>) {
    let mut req = rx.recv().await.expect("incoming request");
    let id = match &req.first {
        EnvelopeObject::Query(s) => s.id.clone(),
        other => panic!("expected query, got {other:?}"),
    };
    req.stream
        .send(&EnvelopeObject::Result(ResultHeader {
            id: id.clone(),
            columns: vec!["answer".to_string()],
            decl_types: Vec::new(),
        }))
        .await
        .expect("send result");
    req.stream
        .send(&EnvelopeObject::Row(vec![Row {
            seq: 0,
            cells: vec![("answer".to_string(), Value::Integer(42))],
        }]))
        .await
        .expect("send row");
    req.stream
        .send(&EnvelopeObject::Done(Done {
            id,
            row_count: Some(1),
            affected_rows: None,
            last_insert_rowid: None,
            truncated: false,
        }))
        .await
        .expect("send done");
    req.stream.finish().await.expect("finish");
    (rx, req)
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_and_request_round_trip() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (server, client, server_rx) = offline_pair().await;
        let server_id = server.peer_id();

        let (mut stream, peer_hello) = client.open(server_id).await.expect("open");
        assert_eq!(peer_hello.envelope_version, "0.1");
        assert!(peer_hello.mods.iter().any(|m| m == "help"));
        assert!(peer_hello.encodings.iter().any(|e| e == "turtle"));

        let serve = tokio::spawn(serve_one(server_rx));

        stream
            .send(&query(
                "01JTESTREQ0000000000000001",
                "sql-sqlite",
                "SELECT 42",
            ))
            .await
            .expect("send query");
        stream.finish().await.expect("finish");

        let (server_rx, served) = serve.await.expect("server task");
        assert_eq!(served.peer, client.peer_id());
        assert_eq!(served.peer_hello.mods, vec!["help"]);

        // Response choreography: Result, Row, Done, clean end of stream.
        match stream.recv().await.expect("recv result") {
            Some(EnvelopeObject::Result(h)) => assert_eq!(h.columns, vec!["answer"]),
            other => panic!("expected result header, got {other:?}"),
        }
        match stream.recv().await.expect("recv row") {
            Some(EnvelopeObject::Row(rows)) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].cells[0], ("answer".to_string(), Value::Integer(42)));
            }
            other => panic!("expected row, got {other:?}"),
        }
        match stream.recv().await.expect("recv done") {
            Some(EnvelopeObject::Done(d)) => assert_eq!(d.row_count, Some(1)),
            other => panic!("expected done, got {other:?}"),
        }
        assert!(stream.recv().await.expect("recv eof").is_none());

        // A second request reuses the cached connection: no new dial.
        assert_eq!(client.dial_count(), 1);
        let serve = tokio::spawn(serve_one(server_rx));
        let (mut stream2, _) = client.open(server_id).await.expect("open again");
        assert_eq!(client.dial_count(), 1, "cached connection must be reused");
        assert_eq!(server.connections_accepted(), 1);
        stream2
            .send(&query(
                "01JTESTREQ0000000000000002",
                "sql-sqlite-3.46",
                "SELECT 42",
            ))
            .await
            .expect("send query 2");
        stream2.finish().await.expect("finish 2");
        while stream2.recv().await.expect("drain 2").is_some() {}
        serve.await.expect("server task 2");

        client.shutdown().await;
        server.shutdown().await;
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn mod_unsupported_fast_fail() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (server, client, mut server_rx) = offline_pair().await;

        let (mut stream, _) = client.open(server.peer_id()).await.expect("open");
        stream
            .send(&query(
                "01JTESTREQ0000000000000009",
                "no-such-mod",
                "whatever",
            ))
            .await
            .expect("send");
        stream.finish().await.expect("finish");

        match stream.recv().await.expect("recv error") {
            Some(EnvelopeObject::Error(e)) => {
                assert_eq!(e.code, ErrorCode::ModUnsupported.as_str());
                assert_eq!(e.id.as_deref(), Some("01JTESTREQ0000000000000009"));
                let reason = e.reason.expect("reason");
                assert!(reason.contains("no-such-mod"), "reason: {reason}");
            }
            other => panic!("expected mod-unsupported error, got {other:?}"),
        }
        assert!(stream.recv().await.expect("recv eof").is_none());

        // The gated request never reached the node.
        assert!(
            server_rx.try_recv().is_err(),
            "fast-failed request must not surface to the node"
        );

        // The versioned advertisement (sql-sqlite-3.46) matches its base tag.
        let (mut stream, _) = client.open(server.peer_id()).await.expect("open 2");
        stream
            .send(&query(
                "01JTESTREQ0000000000000010",
                "sql-sqlite",
                "SELECT 1",
            ))
            .await
            .expect("send");
        stream.finish().await.expect("finish");
        let (_server_rx, served) = serve_one(server_rx).await;
        match &served.first {
            EnvelopeObject::Query(s) => assert_eq!(s.modulation, "sql-sqlite"),
            other => panic!("expected query, got {other:?}"),
        }
        while stream.recv().await.expect("drain").is_some() {}

        client.shutdown().await;
        server.shutdown().await;
    })
    .await
    .expect("test timed out");
}

/// A raw iroh endpoint (no IrohTransport client) probes with plain text,
/// receives the unframed banner, then sends a real hello frame on the same
/// stream and completes the handshake.
#[tokio::test(flavor = "multi_thread")]
async fn plaintext_probe_gets_banner_then_hello_works() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (server, _server_rx) = IrohTransport::bind(IrohConfig::offline(server_hello()))
            .await
            .expect("bind server");

        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr(std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                0,
            )))
            .expect("bind addr")
            .bind()
            .await
            .expect("bind probe endpoint");

        let server_endpoint_id =
            iroh::EndpointId::from_bytes(server.peer_id().as_bytes()).expect("endpoint id");
        let addr = iroh::EndpointAddr::new(server_endpoint_id).with_addrs(
            server
                .direct_addrs()
                .into_iter()
                .map(iroh::TransportAddr::Ip),
        );
        let conn = endpoint.connect(addr, ALPN).await.expect("connect");
        let (mut send, mut recv) = conn.open_bi().await.expect("open bi");

        // Unframed probe: a lone HELP line.
        send.write_all(b"HELP\n").await.expect("write probe");

        // The banner comes back unframed, newline-terminated.
        let mut banner = vec![0u8; PLAINTEXT_BANNER.len()];
        recv.read_exact(&mut banner).await.expect("read banner");
        assert_eq!(
            std::str::from_utf8(&banner).expect("utf8"),
            PLAINTEXT_BANNER
        );
        assert!(PLAINTEXT_BANNER.ends_with('\n'));

        // Same stream still accepts a real hello.
        let mut frame = BytesMut::new();
        encode_envelope(&EnvelopeObject::Hello(client_hello()), &mut frame).expect("encode hello");
        send.write_all(&frame).await.expect("write hello");
        send.finish().expect("finish send");

        let mut buf = BytesMut::new();
        let server_hello_back = loop {
            if let Some(obj) = decode_envelope(&mut buf).expect("decode") {
                break obj;
            }
            let mut chunk = [0u8; 4096];
            match recv.read(&mut chunk).await.expect("read") {
                Some(n) => buf.extend_from_slice(&chunk[..n]),
                None => panic!("stream ended before the server hello"),
            }
        };
        match server_hello_back {
            EnvelopeObject::Hello(h) => {
                assert_eq!(h.envelope_version, "0.1");
                assert!(h.mods.iter().any(|m| m == "help"));
            }
            other => panic!("expected hello after probe, got {other:?}"),
        }

        conn.close(0u32.into(), b"done");
        endpoint.close().await;
        server.shutdown().await;
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn incompatible_hello_is_refused() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (server, _server_rx) = IrohTransport::bind(IrohConfig::offline(server_hello()))
            .await
            .expect("bind server");

        let mut bad_hello = client_hello();
        bad_hello.envelope_version = "9.0".to_string();
        let (client, _rx) = IrohTransport::bind(IrohConfig::offline(bad_hello))
            .await
            .expect("bind client");
        client.add_peer_addrs(server.peer_id(), server.direct_addrs());

        match client.open(server.peer_id()).await {
            Err(TransportError::Refused { code, .. }) => {
                assert_eq!(code, "envelope-version");
            }
            other => panic!("expected refusal, got {other:?}"),
        }

        client.shutdown().await;
        server.shutdown().await;
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_round_trip() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let secret = [42u8; 32];
        let expected_peer = endpoint_id_from_secret(secret);

        // Offline mint: localhost direct addresses only, no network needed.
        let ticket = mint_ticket(secret, true, Duration::from_secs(5))
            .await
            .expect("mint ticket");
        let (peer, addrs) = parse_ticket(&ticket).expect("parse ticket");
        assert_eq!(peer, expected_peer);
        assert!(!addrs.is_empty(), "offline ticket must carry direct addrs");
        assert!(addrs.iter().all(|a| a.ip().is_loopback()));

        // A live transport's ticket parses to its own peer id.
        let (server, _rx) = IrohTransport::bind(IrohConfig::offline(server_hello()))
            .await
            .expect("bind");
        let (peer, _addrs) = parse_ticket(&server.ticket()).expect("parse live ticket");
        assert_eq!(peer, server.peer_id());
        server.shutdown().await;

        // Garbage does not parse.
        assert!(parse_ticket("not-a-ticket").is_err());
    })
    .await
    .expect("test timed out");
}

/// Relay-path round trip over the n0 public relays (production preset, no
/// manual addresses: id-only dial through relay + address lookup). Needs
/// network access, so it is #[ignore]d; M2 acceptance runs it manually:
///
///     cargo test -p resonator-transport --test transport relay_path -- --ignored
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs network access to the n0 relays; run manually for M2 acceptance"]
async fn relay_path_round_trip() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let (server, server_rx) = IrohTransport::bind(IrohConfig::n0(server_hello()))
            .await
            .expect("bind server");
        let (client, _client_rx) = IrohTransport::bind(IrohConfig::n0(client_hello()))
            .await
            .expect("bind client");

        // Give the server time to acquire a home relay and publish it.
        tokio::time::sleep(Duration::from_secs(3)).await;

        let (mut stream, peer_hello) = client.open(server.peer_id()).await.expect("open");
        assert_eq!(peer_hello.envelope_version, "0.1");

        let serve = tokio::spawn(serve_one(server_rx));
        stream
            .send(&query(
                "01JTESTRELAY00000000000001",
                "sql-sqlite",
                "SELECT 42",
            ))
            .await
            .expect("send");
        stream.finish().await.expect("finish");
        serve.await.expect("serve");

        let mut saw_done = false;
        while let Some(obj) = stream.recv().await.expect("recv") {
            if matches!(obj, EnvelopeObject::Done(_)) {
                saw_done = true;
            }
        }
        assert!(saw_done, "expected a Done trailer over the relay path");

        client.shutdown().await;
        server.shutdown().await;
    })
    .await
    .expect("relay test timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn addr_source_supplies_dial_addresses() {
    let (server, _server_rx) = IrohTransport::bind(IrohConfig::offline(server_hello()))
        .await
        .expect("bind server");
    let (client, _client_rx) = IrohTransport::bind(IrohConfig::offline(client_hello()))
        .await
        .expect("bind client");

    // No add_peer_addrs: offline dialing can only succeed through the
    // live source, installed after bind and consulted at dial time (the
    // `rsntr peer add`-while-serving shape).
    let server_addrs = server.direct_addrs();
    client.set_addr_source(Arc::new(move |_peer| {
        let addrs = server_addrs.clone();
        Box::pin(async move { addrs })
            as std::pin::Pin<
                Box<dyn std::future::Future<Output = Vec<std::net::SocketAddr>> + Send>,
            >
    }));

    let (mut stream, peer_hello) =
        tokio::time::timeout(TEST_TIMEOUT, client.open(server.peer_id()))
            .await
            .expect("dial within deadline")
            .expect("open through the addr source");
    assert_eq!(peer_hello.mods, server_hello().mods);
    stream.finish().await.expect("finish");

    client.shutdown().await;
    server.shutdown().await;
}

/// A live connection reports the peer's observed direct addresses to
/// the addr sink on both ends: dialer learns the server's, acceptor
/// learns the dialer's. This is what keeps `_peers.addrs` fresh across
/// peer restarts.
#[tokio::test(flavor = "multi_thread")]
async fn addr_sink_reports_live_direct_addresses() {
    use std::net::SocketAddr;
    use std::sync::Mutex;

    let (server, client, server_rx) = offline_pair().await;

    let seen_by_client: Arc<Mutex<Vec<(resonator_transport::PeerId, Vec<SocketAddr>)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let seen_by_server = seen_by_client.clone();
    let by_client = seen_by_client.clone();
    client.set_addr_sink(Arc::new(move |peer, addrs| {
        by_client.lock().unwrap().push((peer, addrs));
    }));
    let server_seen: Arc<Mutex<Vec<(resonator_transport::PeerId, Vec<SocketAddr>)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let by_server = server_seen.clone();
    server.set_addr_sink(Arc::new(move |peer, addrs| {
        by_server.lock().unwrap().push((peer, addrs));
    }));
    drop(seen_by_server);

    let (mut stream, _hello) = tokio::time::timeout(TEST_TIMEOUT, client.open(server.peer_id()))
        .await
        .expect("dial within deadline")
        .expect("open");
    stream.send(&query("01JADDRSINK000000000000001", "help", "")).await.expect("send");
    stream.finish().await.expect("finish");
    let (_rx, _req) = serve_one(server_rx).await;

    // Both watchers sample immediately on spawn; give them a moment.
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let client_ok = seen_by_client
            .lock()
            .unwrap()
            .iter()
            .any(|(p, a)| *p == server.peer_id() && a.iter().all(|sa| sa.ip().is_loopback()) && !a.is_empty());
        let server_ok = server_seen
            .lock()
            .unwrap()
            .iter()
            .any(|(p, a)| *p == client.peer_id() && !a.is_empty());
        if client_ok && server_ok {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "sinks not called in time; client saw {:?}, server saw {:?}",
            seen_by_client.lock().unwrap(),
            server_seen.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    client.shutdown().await;
    server.shutdown().await;
}
