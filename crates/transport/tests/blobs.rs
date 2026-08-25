//! iroh-blobs on the shared endpoint: add, peer-gated provide, verified
//! fetch over localhost iroh (chat protocol sec 6).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use resonator_protocol::Hello;
use resonator_transport::{
    BlobsConfig, IrohConfig, IrohTransport, PeerId, TransportError, basic_hello, parse_blob_hash,
};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "rsntr-blobs-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        std::fs::create_dir_all(&path).expect("creating temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn hello() -> Hello {
    basic_hello(&["help"], None)
}

/// An offline transport with a blob store, gated to `allowed` (or open
/// when `None`).
async fn bind_provider(store_dir: &Path, allowed: Option<PeerId>) -> Arc<IrohTransport> {
    let gate = allowed.map(|ok_peer| {
        let gate: resonator_transport::BlobGate = Arc::new(move |peer: PeerId| {
            Box::pin(async move { peer == ok_peer })
                as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        });
        gate
    });
    let config = IrohConfig {
        hello: hello(),
        secret_key: None,
        offline: true,
        gossip: false,
        blobs: Some(BlobsConfig {
            store_dir: store_dir.to_path_buf(),
            gate,
        }),
    };
    let (transport, _rx) = IrohTransport::bind(config).await.expect("bind provider");
    transport
}

async fn bind_client() -> Arc<IrohTransport> {
    let (transport, _rx) = IrohTransport::bind(IrohConfig::offline(hello()))
        .await
        .expect("bind client");
    transport
}

/// 1 MiB of deterministic pseudo-random bytes.
fn test_bytes() -> Vec<u8> {
    let mut state: u64 = 0x2545F4914F6CDD1D;
    (0..1024 * 1024)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn add_fetch_round_trip_with_verification() {
    let tmp = TempDir::new("roundtrip");
    let src = tmp.path().join("seedlings.bin");
    let data = test_bytes();
    std::fs::write(&src, &data).expect("write source");

    // The client's id must be known before the provider's gate exists.
    let client = bind_client().await;
    let provider = bind_provider(&tmp.path().join("blobs"), Some(client.peer_id())).await;

    let (hash, size) = provider.blob_add_path(&src).await.expect("add");
    assert_eq!(size, data.len() as u64);
    assert_eq!(hash.len(), 64);
    // The hash is the real BLAKE3 of the content.
    assert_eq!(hash, blake3_hex(&data));

    client.add_peer_addrs(provider.peer_id(), provider.direct_addrs());

    // Fetch to a path; the exported bytes are the original ones.
    let out = tmp.path().join("fetched.bin");
    let n = client
        .blob_fetch_to_path(provider.peer_id(), &format!("blake3:{hash}"), &out)
        .await
        .expect("fetch to path");
    assert_eq!(n, data.len() as u64);
    assert_eq!(std::fs::read(&out).expect("read fetched"), data);

    // Fetch to memory too (the stdout path).
    let bytes = client
        .blob_fetch_bytes(provider.peer_id(), &hash)
        .await
        .expect("fetch bytes");
    assert_eq!(bytes, data);

    // A hash the provider does not have fails; it cannot be conjured.
    let missing = "00".repeat(32);
    let err = client
        .blob_fetch_bytes(provider.peer_id(), &missing)
        .await
        .expect_err("missing hash must fail");
    assert!(matches!(err, TransportError::Blobs(_)), "got {err:?}");

    client.shutdown().await;
    provider.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_gate_refuses_unadmitted_peers() {
    let tmp = TempDir::new("gate");
    let src = tmp.path().join("secret.bin");
    std::fs::write(&src, b"gated bytes").expect("write source");

    let admitted = bind_client().await;
    let stranger = bind_client().await;
    let provider = bind_provider(&tmp.path().join("blobs"), Some(admitted.peer_id())).await;
    let (hash, _size) = provider.blob_add_path(&src).await.expect("add");

    admitted.add_peer_addrs(provider.peer_id(), provider.direct_addrs());
    stranger.add_peer_addrs(provider.peer_id(), provider.direct_addrs());

    let ok = admitted
        .blob_fetch_bytes(provider.peer_id(), &hash)
        .await
        .expect("admitted fetch");
    assert_eq!(ok, b"gated bytes");

    let err = stranger
        .blob_fetch_bytes(provider.peer_id(), &hash)
        .await
        .expect_err("stranger must be refused");
    assert!(matches!(err, TransportError::Blobs(_)), "got {err:?}");

    admitted.shutdown().await;
    stranger.shutdown().await;
    provider.shutdown().await;
}

#[test]
fn hash_parsing_accepts_prefix_and_rejects_garbage() {
    let hex = "af".repeat(32);
    assert!(parse_blob_hash(&hex).is_ok());
    assert!(parse_blob_hash(&format!("blake3:{hex}")).is_ok());
    assert!(parse_blob_hash("blake3:short").is_err());
    assert!(parse_blob_hash("not a hash").is_err());
}

/// Independent BLAKE3 of `data` (iroh-blobs' own hasher, computed
/// without any store).
fn blake3_hex(data: &[u8]) -> String {
    iroh_blobs::Hash::new(data).to_hex()
}

#[tokio::test(flavor = "multi_thread")]
async fn blob_import_reports_a_held_store_instead_of_hanging() {
    let tmp = TempDir::new("import-held");
    let store_dir = tmp.path().join("blobs");
    // The provider holds the exclusive store open, as a serving node
    // does; the side-door import must error promptly, never hang.
    let provider = bind_provider(&store_dir, None).await;
    let file = tmp.path().join("note.txt");
    std::fs::write(&file, b"attachment bytes").expect("write file");

    let started = std::time::Instant::now();
    let res = resonator_transport::blob_import(&store_dir, &file).await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "import against a held store must return promptly"
    );
    let err = res.expect_err("import against a held store must error");
    assert!(matches!(err, TransportError::Blobs(_)), "got {err:?}");

    provider.shutdown().await;
}
