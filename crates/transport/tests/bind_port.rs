//! Pinned UDP ports: a node keeps the same port across restarts, so a
//! firewall rule written before it starts keeps pointing at it.

use std::net::UdpSocket;

use resonator_protocol::Hello;
use resonator_transport::{BindPorts, IrohConfig, IrohTransport};

fn hello() -> Hello {
    resonator_transport::basic_hello(&["help"], Some("bind-port test"))
}

/// A port nothing holds right now: bound and released, so the test does
/// not hard-code a number that might be in use on the machine.
fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .expect("probe socket")
        .local_addr()
        .expect("probe addr")
        .port()
}

/// The ports of the endpoint's IPv4 sockets. The endpoint also opens an
/// IPv6 socket, which is pinned separately.
fn v4_ports(transport: &IrohTransport) -> Vec<u16> {
    transport
        .direct_addrs()
        .iter()
        .filter(|a| a.is_ipv4())
        .map(|a| a.port())
        .collect()
}

fn config(port: u16, secret: [u8; 32]) -> IrohConfig {
    IrohConfig {
        secret_key: Some(secret),
        bind_ports: BindPorts {
            v4: Some(port),
            v6: None,
        },
        ..IrohConfig::offline(hello())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pinned_port_is_the_same_after_a_restart() {
    let port = free_port();
    let secret = [7u8; 32];

    let mut seen = Vec::new();
    for _ in 0..2 {
        let (transport, _rx) = IrohTransport::bind(config(port, secret))
            .await
            .expect("bind with a pinned port");
        seen.push(v4_ports(&transport));
        transport.shutdown().await;
    }

    assert_eq!(
        seen[0],
        vec![port],
        "the endpoint bound the port it was given"
    );
    assert_eq!(
        seen[0], seen[1],
        "the same port comes back after a restart, which is the point"
    );
}

/// The endpoint always opens an IPv6 socket as well, and pinning IPv4
/// leaves that one ephemeral. A node whose whole address must be stable
/// pins both; a firewall rule for IPv4 only needs the one.
#[tokio::test(flavor = "multi_thread")]
async fn each_family_is_pinned_separately() {
    let (v4, v6) = (free_port(), free_port());
    let both = IrohConfig {
        secret_key: Some([10u8; 32]),
        bind_ports: BindPorts {
            v4: Some(v4),
            v6: Some(v6),
        },
        ..IrohConfig::offline(hello())
    };
    let (transport, _rx) = IrohTransport::bind(both).await.expect("bind both");
    let ports: Vec<u16> = transport.direct_addrs().iter().map(|a| a.port()).collect();
    assert!(ports.contains(&v4) && ports.contains(&v6), "{ports:?}");
    transport.shutdown().await;

    // Pinning only IPv4 leaves the IPv6 socket on an ephemeral port.
    let (transport, _rx) = IrohTransport::bind(config(free_port(), [12u8; 32]))
        .await
        .expect("bind v4 only");
    let v6_socket = transport
        .direct_addrs()
        .into_iter()
        .find(|a| a.is_ipv6())
        .expect("an IPv6 socket is always opened");
    assert_ne!(v6_socket.port(), 0);
    transport.shutdown().await;
}

/// The control for the test above: without a pin the port moves, so the
/// equality there is the setting working rather than luck.
#[tokio::test(flavor = "multi_thread")]
async fn an_unpinned_port_moves_between_restarts() {
    let secret = [8u8; 32];
    let mut seen = Vec::new();
    for _ in 0..2 {
        let (transport, _rx) = IrohTransport::bind(IrohConfig {
            secret_key: Some(secret),
            ..IrohConfig::offline(hello())
        })
        .await
        .expect("bind");
        seen.push(v4_ports(&transport)[0]);
        transport.shutdown().await;
    }
    assert_ne!(
        seen[0], seen[1],
        "an ephemeral port is not expected to repeat"
    );
}

/// Taking the port must fail loudly and name it. Landing on some other
/// port instead would leave the firewall rule pointing at nothing, which
/// is the failure the setting exists to prevent.
#[tokio::test(flavor = "multi_thread")]
async fn a_port_already_held_fails_and_says_which() {
    let port = free_port();
    let _held = UdpSocket::bind(("127.0.0.1", port)).expect("hold the port");

    let Err(err) = IrohTransport::bind(config(port, [9u8; 32])).await else {
        panic!("binding a held port fails");
    };
    let err = err.to_string();
    assert!(err.contains(&port.to_string()), "{err}");
}
