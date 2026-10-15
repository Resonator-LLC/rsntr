//! A client endpoint bound with `publish: false` still binds and dials
//! (resolve-only address lookup); offline here, where publishing is
//! moot, so the shape compiles and the plumbing is exercised.

use resonator_protocol::Hello;
use resonator_transport::{IrohConfig, IrohTransport};

fn hello() -> Hello {
    resonator_transport::basic_hello(&["help"], Some("test"))
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_false_binds_offline() {
    let config = IrohConfig {
        bind_ports: Default::default(),
        publish: false,
        ..IrohConfig::offline(hello())
    };
    let (transport, _incoming) = IrohTransport::bind(config).await.expect("bind");
    assert!(!transport.direct_addrs().is_empty());
    transport.shutdown().await;
}
