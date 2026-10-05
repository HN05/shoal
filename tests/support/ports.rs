//! Ephemeral ports the daemon's availability probe sees exactly as the test does.
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
};

/// An ephemeral port held on both wildcard addresses until dropped.
///
/// The daemon probes `0.0.0.0` and `[::]`, so a port picked on loopback alone
/// can still be bound on another address and refuse allocation after the test
/// releases its listener. Sockets skip SO_REUSEADDR like the probe: with it,
/// macOS can hand out a wildcard port another socket holds on loopback.
pub struct HeldPort {
    pub port: u16,
    _ipv4: Socket,
    _ipv6: Option<Socket>,
}

pub fn hold() -> HeldPort {
    loop {
        let ipv4 = listen(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).unwrap();
        let port = ipv4.local_addr().unwrap().as_socket().unwrap().port();
        if let Some(ipv6) = hold_ipv6(port) {
            return HeldPort {
                port,
                _ipv4: ipv4,
                _ipv6: ipv6,
            };
        }
    }
}

/// `None` when the IPv6 wildcard is taken; `Some(None)` when the host has no
/// IPv6, which the daemon's probe also treats as free.
fn hold_ipv6(port: u16) -> Option<Option<Socket>> {
    match listen(SocketAddr::from((Ipv6Addr::UNSPECIFIED, port))) {
        Ok(socket) => Some(Some(socket)),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => None,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::AddrNotAvailable | io::ErrorKind::Unsupported
            ) || error.raw_os_error() == Some(libc::EAFNOSUPPORT) =>
        {
            Some(None)
        }
        Err(error) => panic!("hold IPv6 wildcard port {port}: {error}"),
    }
}

fn listen(address: SocketAddr) -> io::Result<Socket> {
    let socket = Socket::new(
        Domain::for_address(address),
        Type::STREAM,
        Some(Protocol::TCP),
    )?;
    if address.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&address.into())?;
    socket.listen(1)?;
    Ok(socket)
}
