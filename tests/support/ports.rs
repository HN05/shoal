//! Ports the daemon's availability probe sees exactly as the test does.
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::Range,
};

/// A port held on both wildcard addresses until dropped.
///
/// The daemon probes `0.0.0.0` and `[::]`, so the test holds both. Sockets
/// skip SO_REUSEADDR like the probe: with it, macOS can bind a wildcard port
/// another socket holds on loopback. Ports come from below the ephemeral
/// ranges (Linux 32768, macOS 49152) and the built-in Shoal range, so once
/// released the kernel does not hand them to another process's `bind(0)` or
/// outgoing connection before the daemon probes them.
pub struct HeldPort {
    pub port: u16,
    _ipv4: Socket,
    _ipv6: Option<Socket>,
}

const RANGE: Range<u16> = 20000..32768;

pub fn hold() -> HeldPort {
    // Start at a random offset so concurrent tests rarely probe the same ports.
    let offset = RandomState::new().build_hasher().finish() as usize;
    let span = RANGE.len();
    for step in 0..span {
        let port = RANGE.start + ((offset + step) % span) as u16;
        let ipv4 = match listen(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))) {
            Ok(socket) => socket,
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
            Err(error) => panic!("hold IPv4 wildcard port {port}: {error}"),
        };
        if let Some(ipv6) = hold_ipv6(port) {
            return HeldPort {
                port,
                _ipv4: ipv4,
                _ipv6: ipv6,
            };
        }
    }
    panic!("no free port in {RANGE:?}");
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
