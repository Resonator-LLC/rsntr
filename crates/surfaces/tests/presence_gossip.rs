//! Presence acceptance: two in-process nodes gossiping offline (loopback
//! only, no relays). Beacons propagate and each node refreshes the other's
//! `_peers.last_seen`; a silenced node goes stale past the threshold.

use std::time::Duration;

use rusqlite::{Connection, params};

use resonator_node::DbHandle;
use resonator_surfaces::presence::{Presence, PresenceConfig, is_stale};
use resonator_transport::PeerId;

/// Fast cadence so propagation and staleness happen in seconds.
const CADENCE: Duration = Duration::from_millis(150);
/// Staleness threshold used by the assertions.
const THRESHOLD: Duration = Duration::from_secs(2);

fn make_db(peers: &[PeerId]) -> DbHandle {
    let conn = Connection::open_in_memory().expect("open db");
    conn.execute_batch(resonator_node::PEERS_DDL)
        .expect("peers ddl");
    for p in peers {
        conn.execute(
            "INSERT INTO _peers (endpoint_id, added_at) VALUES (?1, datetime('now'))",
            params![p.to_string()],
        )
        .expect("seed peer");
    }
    DbHandle::spawn(conn)
}

fn peer_id_of(secret: &[u8; 32]) -> PeerId {
    let sk = iroh::SecretKey::from_bytes(secret);
    PeerId(*sk.public().as_bytes())
}

async fn wait_until<F>(deadline: Duration, mut cond: F) -> bool
where
    F: AsyncFnMut() -> bool,
{
    let start = std::time::Instant::now();
    loop {
        if cond().await {
            return true;
        }
        if start.elapsed() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn fresh(db: &DbHandle, peer: PeerId) -> bool {
    db.call(move |conn| !is_stale(conn, &peer, THRESHOLD).expect("is_stale"))
        .await
        .expect("db call")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn presence_propagates_and_silence_goes_stale() {
    // Two identities known up front (secret -> deterministic public key).
    let secrets: [[u8; 32]; 2] = [[11u8; 32], [22u8; 32]];
    let ids: [PeerId; 2] = [peer_id_of(&secrets[0]), peer_id_of(&secrets[1])];

    // Each node's _peers holds the other; presence only refreshes admitted
    // peers.
    let dbs = [make_db(&[ids[1]]), make_db(&[ids[0]])];
    let mut services = Vec::new();
    for (i, secret) in secrets.iter().enumerate() {
        let p = Presence::start(
            PresenceConfig {
                cadence: CADENCE,
                status: Some("around".to_string()),
                offline: true,
                secret_key: Some(*secret),
            },
            dbs[i].clone(),
        )
        .await
        .expect("start presence");
        assert_eq!(p.peer_id(), ids[i]);
        services.push(p);
    }

    // Exchange dial hints (offline: no discovery), then join the shared
    // peer-set topic; the admitted set comes from _peers.
    let addrs = [services[0].endpoint_addr(), services[1].endpoint_addr()];
    services[0].register_peer(addrs[1].clone());
    services[1].register_peer(addrs[0].clone());
    for (i, svc) in services.iter().enumerate() {
        let admitted = svc.admitted_peers().await.expect("admitted");
        assert_eq!(admitted, vec![ids[1 - i]]);
        svc.join(admitted).await.expect("join topic");
    }

    // 1. Presence propagates: each node sees the other as fresh.
    let propagated = wait_until(Duration::from_secs(20), async || {
        fresh(&dbs[0], ids[1]).await && fresh(&dbs[1], ids[0]).await
    })
    .await;
    assert!(
        propagated,
        "presence did not propagate between the two nodes"
    );

    // 2. Node 1 goes silent; node 0 reports it stale past the threshold.
    services[1].set_beaconing(false);
    assert!(!services[1].is_beaconing());
    let went_stale = wait_until(Duration::from_secs(15), async || {
        !fresh(&dbs[0], ids[1]).await
    })
    .await;
    assert!(went_stale, "silenced node never went stale");

    // 3. Beacons resume; node 0 sees it fresh again.
    services[1].set_beaconing(true);
    let back = wait_until(Duration::from_secs(15), async || {
        fresh(&dbs[0], ids[1]).await
    })
    .await;
    assert!(back, "resumed node never became fresh again");

    for svc in &services {
        svc.shutdown().await;
    }
}
