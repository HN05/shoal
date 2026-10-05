//! Ephemeral ports the daemon's availability probe sees exactly as the test does.
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener},
};

/// An ephemeral port held on both wildcard addresses until dropped.
///
/// The daemon probes `0.0.0.0` and `[::]`, so a port picked on loopback alone
/// can still be bound on another address and refuse allocation after the test
/// releases its listener.
pub struct HeldPort {
    pub port: u16,
    _ipv4: TcpListener,
    _ipv6: Option<socket2::Socket>,
}

pub fn hold() -> HeldPort {
    loop {
        let ipv4 = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = ipv4.local_addr().unwrap().port();
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
fn hold_ipv6(port: u16) -> Option<Option<socket2::Socket>> {
    let bind = || -> io::Result<socket2::Socket> {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV6,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )?;
        socket.set_only_v6(true)?;
        socket.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)).into())?;
        socket.listen(1)?;
        Ok(socket)
    };
    match bind() {
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
