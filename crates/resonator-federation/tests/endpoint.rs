//! Two in-process federation endpoints, discovery and relays off, talking
//! over loopback with static addresses.

use std::sync::Arc;
use std::time::Duration;

use resonator_federation::dns::StaticTxtResolver;
use resonator_federation::endpoint::{
    EchoHandler, EndpointOptions, FederationEndpoint, KeyDomainAuthority, LocalIdentity,
    RequestError, new_request_id,
};
use resonator_federation::service::ServerHandler;
use resonator_federation::{ALPN, Authorisation, Policy, SecretKey, Trust, key_domain};
use resonator_model::proto::federation_error::federation_error::Code;
use resonator_model::proto::federation_rpc::{
    Echo, Probe, ProbeResult, federation_request, federation_response,
};
use resonator_server::account::HumanAccountData;
use resonator_server::clock::SystemClock;
use resonator_server::storage::{Database, DatabaseOptions};

fn options() -> EndpointOptions {
    EndpointOptions {
        request_timeout: Duration::from_secs(5),
        ..EndpointOptions::local()
    }
}

/// An endpoint speaking for its own key domain, answering with echoes.
async fn key_domain_endpoint() -> FederationEndpoint {
    let key = SecretKey::generate();
    let identity = LocalIdentity {
        domains: vec![key_domain(&key.public())],
        capabilities: vec!["echo".to_owned()],
        rotations: Vec::new(),
    };
    FederationEndpoint::bind(
        key,
        options(),
        identity,
        Arc::new(KeyDomainAuthority),
        Arc::new(EchoHandler),
    )
    .await
    .unwrap()
}

fn echo(payload: &[u8]) -> federation_request::Op {
    federation_request::Op::Echo(Echo {
        payload: Some(payload.to_vec()),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_then_requests_on_their_own_streams() {
    let a = key_domain_endpoint().await;
    let b = key_domain_endpoint().await;
    assert_eq!(ALPN, b"resonator/fed/1");
    a.add_peer_addr(b.addr());
    let b_domain = key_domain(&b.id());

    // The Hello: B speaks for its key domain, as its handshake proved.
    let peer = a.connect(b.id(), Some(&b_domain)).await.unwrap();
    assert_eq!(peer.key, b.id());
    assert_eq!(peer.domains, std::slice::from_ref(&b_domain));
    assert_eq!(peer.hello.protocol_version, Some(1));
    assert_eq!(peer.hello.capability, ["echo"]);

    // An echo, and a probe (the echo handler knows nobody).
    let answer = a
        .request(b.id(), Some(&b_domain), echo(b"ping"))
        .await
        .unwrap();
    assert_eq!(
        answer,
        federation_response::Result::Echo(Echo {
            payload: Some(b"ping".to_vec())
        })
    );
    let answer = a
        .request(
            b.id(),
            None,
            federation_request::Op::Probe(Probe {
                address: Some(format!("alice@{b_domain}")),
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        answer,
        federation_response::Result::Probe(ProbeResult {
            exists: Some(false)
        })
    );

    // Many requests at once share the cached connection.
    let requests = (0..32u8).map(|i| {
        let a = a.clone();
        let key = b.id();
        async move { a.request(key, None, echo(&[i; 100])).await }
    });
    let answers = futures_join_all(requests).await;
    for (i, answer) in answers.into_iter().enumerate() {
        assert_eq!(
            answer.unwrap(),
            federation_response::Result::Echo(Echo {
                payload: Some(vec![i as u8; 100])
            })
        );
    }

    // A megabyte goes through; more than a frame holds does not.
    let big = vec![42u8; 1 << 20];
    assert_eq!(
        a.request(b.id(), None, echo(&big)).await.unwrap(),
        federation_response::Result::Echo(Echo { payload: Some(big) })
    );
    let too_big = vec![0u8; 5 << 20];
    assert!(matches!(
        a.request(b.id(), None, echo(&too_big)).await,
        Err(RequestError::Protocol { .. })
    ));

    // B is not authorised for a domain its key is not bound to.
    assert!(matches!(
        a.connect(b.id(), Some("b.example")).await,
        Err(RequestError::Unauthorised {
            verdict: Authorisation::NotBound,
            ..
        })
    ));

    // B in turn dials A.
    b.add_peer_addr(a.addr());
    let peer = b.connect(a.id(), Some(&key_domain(&a.id()))).await.unwrap();
    assert_eq!(peer.domains, [key_domain(&a.id())]);

    // After B shuts down, requests to it fail and A shuts down cleanly.
    b.shutdown().await;
    assert!(a.request(b.id(), None, echo(b"gone")).await.is_err());
    a.shutdown().await;
    assert!(matches!(
        a.request(b.id(), None, echo(b"x")).await,
        Err(RequestError::Closed)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hello_for_domains_the_key_is_not_bound_to_is_refused() {
    let b = key_domain_endpoint().await;
    let key = SecretKey::generate();
    let impostor = FederationEndpoint::bind(
        key,
        options(),
        LocalIdentity {
            // Claims B's domain and a DNS domain; bound to neither.
            domains: vec![key_domain(&b.id()), "c.example".to_owned()],
            ..LocalIdentity::default()
        },
        Arc::new(KeyDomainAuthority),
        Arc::new(EchoHandler),
    )
    .await
    .unwrap();
    impostor.add_peer_addr(b.addr());
    match impostor.connect(b.id(), None).await {
        Err(RequestError::Refused { error, .. }) => {
            assert_eq!(error.code(), Code::NotAuthorized);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    impostor.shutdown().await;
    b.shutdown().await;
}

/// A server with a database, a trust policy over fake DNS, and the wave
/// server's request handler.
struct Server {
    _dir: tempfile::TempDir,
    db: Database,
    endpoint: FederationEndpoint,
}

async fn server(domain: &str, policy: Policy, dns: &StaticTxtResolver) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("wave.db"), DatabaseOptions::default()).unwrap();
    let key = SecretKey::generate();
    dns.set_keys(domain, [&key.public()]);
    let trust = Trust::new(
        db.federation_store(),
        Arc::new(dns.clone()),
        policy,
        [domain.to_owned()],
        Arc::new(SystemClock),
    );
    let endpoint = FederationEndpoint::bind(
        key,
        options(),
        LocalIdentity {
            domains: vec![domain.to_owned()],
            ..LocalIdentity::default()
        },
        Arc::new(trust),
        Arc::new(ServerHandler::new(db.account_store(), domain)),
    )
    .await
    .unwrap();
    Server {
        _dir: dir,
        db,
        endpoint,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dns_domains_probe_each_other_within_the_allowlist() {
    let dns = StaticTxtResolver::new();
    let a = server("a.example", Policy::allowlist(["b.example"]), &dns).await;
    let b = server("b.example", Policy::allowlist(["a.example"]), &dns).await;
    let c = server("c.example", Policy::open(), &dns).await;
    a.db.account_store()
        .put_account(
            HumanAccountData::new(
                resonator_model::id::ParticipantId::of("alice@a.example").unwrap(),
            )
            .into(),
        )
        .await
        .unwrap();
    for dialler in [&b, &c] {
        dialler.endpoint.add_peer_addr(a.endpoint.addr());
    }

    let probe = |address: &str| {
        federation_request::Op::Probe(Probe {
            address: Some(address.to_owned()),
        })
    };
    let exists = |answer| match answer {
        federation_response::Result::Probe(ProbeResult { exists }) => exists.unwrap(),
        other => panic!("not a probe result: {other:?}"),
    };
    let a_key = a.endpoint.id();
    assert!(exists(
        b.endpoint
            .request(a_key, Some("a.example"), probe("alice@a.example"))
            .await
            .unwrap()
    ));
    assert!(!exists(
        b.endpoint
            .request(a_key, Some("a.example"), probe("bob@a.example"))
            .await
            .unwrap()
    ));
    // Both sides pinned each other on first use.
    let pinned = |db: &Database, domain: &'static str| {
        let store = db.federation_store();
        async move { store.trusted_key(domain).await.unwrap().map(|pin| pin.key) }
    };
    assert_eq!(
        pinned(&b.db, "a.example").await,
        Some(a_key.as_bytes().to_vec())
    );
    assert_eq!(
        pinned(&a.db, "b.example").await,
        Some(b.endpoint.id().as_bytes().to_vec())
    );

    // C is bound to its domain but not on A's allowlist.
    match c
        .endpoint
        .request(a_key, None, probe("alice@a.example"))
        .await
    {
        Err(RequestError::Refused { error, .. }) => assert_eq!(error.code(), Code::NotAuthorized),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(!new_request_id().is_empty());

    for server in [a, b, c] {
        server.endpoint.shutdown().await;
        server.db.close().await;
    }
}

/// Joins futures without pulling in a futures crate.
async fn futures_join_all<F, T>(futures: impl IntoIterator<Item = F>) -> Vec<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handles: Vec<_> = futures.into_iter().map(tokio::spawn).collect();
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    results
}
