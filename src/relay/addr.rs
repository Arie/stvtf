//! Clients' addresses as they are logged.

use std::net::SocketAddr;

// Logged through `tracing`: info has a line per client, jump or failure; debug
// (`RUST_LOG=stvtf::relay=debug`) also handshakes, signon timing and what each client is sent.

/// A client's address as it is logged: the end of the IP hidden, the port kept to tell clients
/// apart.
pub(crate) fn masked_addr(addr: SocketAddr) -> String {
    match addr.ip() {
        std::net::IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            format!("{a}.{b}.{c}.x:{}", addr.port())
        }
        std::net::IpAddr::V6(ip) => {
            format!("{:x}:{:x}:{:x}::x:{}", ip.segments()[0], ip.segments()[1], ip.segments()[2], addr.port())
        }
    }
}
