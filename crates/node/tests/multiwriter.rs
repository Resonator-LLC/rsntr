//! M1: two pipelines writing one file database concurrently never see a
//! busy/snapshot error: write transactions are immediate, so the second
//! writer queues on the busy timeout.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use resonator_authenticator::Chain;
use resonator_node::{DbHandle, Node, NodeConfig, open_node_db, seed_rsntr_defaults};
use resonator_protocol::{EnvelopeObject, Request, RequestKind};
use resonator_transport::{RequestStream, TransportError};

#[derive(Clone, Default)]
struct Recorded {
    sent: Arc<Mutex<Vec<EnvelopeObject>>>,
}

struct LocalStream {
    out: Recorded,
    incoming: VecDeque<EnvelopeObject>,
}

impl RequestStream for LocalStream {
    async fn send(&mut self, obj: &EnvelopeObject) -> Result<(), TransportError> {
        self.out.sent.lock().unwrap().push(obj.clone());
        Ok(())
    }
    async fn recv(&mut self) -> Result<Option<EnvelopeObject>, TransportError> {
        Ok(self.incoming.pop_front())
    }
    async fn finish(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

async fn writer(node: Arc<Node>, tag: &'static str, n: usize) -> (usize, Vec<String>) {
    let mut done = 0;
    let mut errors = Vec::new();
    for i in 0..n {
        let req = Request::new(
            RequestKind::Execute,
            "sql-sqlite",
            format!("INSERT INTO log (who, seq) VALUES ('{tag}', {i})"),
        );
        let out = Recorded::default();
        let mut stream = LocalStream {
            out: out.clone(),
            incoming: VecDeque::new(),
        };
        node.handle_owner(req.to_envelope(), &mut stream)
            .await
            .expect("dispatch");
        for f in out.sent.lock().unwrap().iter() {
            match f {
                EnvelopeObject::Done(_) => done += 1,
                EnvelopeObject::Error(e) => errors.push(e.reason.clone().unwrap_or_default()),
                EnvelopeObject::Denied(d) => errors.push(d.reason.clone().unwrap_or_default()),
                _ => {}
            }
        }
    }
    (done, errors)
}

#[tokio::test(flavor = "multi_thread")]
async fn two_pipelines_interleave_writes_without_busy_errors() {
    let dir = std::env::temp_dir().join(format!("rsntr-multiwriter-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("n.db");
    let mk = |seed: bool| {
        let conn = open_node_db(&path).expect("open");
        if seed {
            seed_rsntr_defaults(&conn).unwrap();
            resonator_node::set_rsntr(&conn, "endpoint_id", &"ee".repeat(32)).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS log (id INTEGER PRIMARY KEY, who TEXT, seq INTEGER)",
            )
            .unwrap();
        }
        Arc::new(Node::new(
            DbHandle::spawn(conn),
            Chain::with_builtin_tiers(),
            NodeConfig::default(),
        ))
    };
    let a = mk(true);
    let b = mk(false);
    a.install_vibration_hook().await.unwrap();
    b.install_vibration_hook().await.unwrap();
    // Raw third writer beside the two pipelines (an app using sqlite
    // directly), hammering in a blocking thread.
    let raw_path = path.clone();
    let raw = tokio::task::spawn_blocking(move || {
        let c = rusqlite::Connection::open(&raw_path).unwrap();
        c.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        let mut failures = 0;
        for i in 0..300 {
            if c.execute("INSERT INTO log (who, seq) VALUES ('raw', ?1)", [i as i64])
                .is_err()
            {
                failures += 1;
            }
        }
        failures
    });
    let (ra, rb, raw_failures) = tokio::join!(
        writer(a.clone(), "a", 300),
        writer(b.clone(), "b", 300),
        raw
    );
    assert_eq!(ra.1, Vec::<String>::new(), "a saw errors");
    assert_eq!(rb.1, Vec::<String>::new(), "b saw errors");
    assert_eq!(ra.0, 300);
    assert_eq!(rb.0, 300);
    assert_eq!(raw_failures.unwrap(), 0);
    let total: i64 = a
        .db()
        .call(|c| {
            c.query_row("SELECT count(*) FROM log", [], |r| r.get(0))
                .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(total, 900);
    let applied: i64 = a
        .db()
        .call(|c| {
            c.query_row("SELECT count(*) FROM _applied", [], |r| r.get(0))
                .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(applied, 600, "every pipeline write recorded once");
    let _ = std::fs::remove_dir_all(&dir);
}
