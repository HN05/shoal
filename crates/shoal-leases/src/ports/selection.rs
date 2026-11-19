use crate::model::PortReservation;
use std::ops::RangeInclusive;

/// Search one allocation's SQLite snapshot; never share membership across claims.
pub(super) fn first_available<E>(
    reservations: &[PortReservation],
    mut range: RangeInclusive<u16>,
    mut probe: impl FnMut(u16) -> Result<bool, E>,
) -> Result<Option<u16>, E> {
    // Keep small lists and searches ending at an early gap allocation-free.
    if reservations.len() <= 32 {
        return scan(reservations, range, probe);
    }
    let first = scan(reservations, range.by_ref().take(2), &mut probe)?;
    if first.is_some() || range.is_empty() {
        return Ok(first);
    }

    // Two scans plus construction and constant-time lookups cost O(P + R).
    // The full u16 domain costs 8 KiB, regardless of the configured range.
    let mut bits = vec![0u64; 1024];
    for reservation in reservations {
        let port = usize::from(reservation.port);
        bits[port / 64] |= 1 << (port % 64);
    }
    for port in range {
        let index = usize::from(port);
        if bits[index / 64] & (1 << (index % 64)) == 0 && probe(port)? {
            return Ok(Some(port));
        }
    }
    Ok(None)
}

fn scan<E>(
    reservations: &[PortReservation],
    candidates: impl IntoIterator<Item = u16>,
    mut probe: impl FnMut(u16) -> Result<bool, E>,
) -> Result<Option<u16>, E> {
    for port in candidates {
        if !reservations.iter().any(|p| p.port == port) && probe(port)? {
            return Ok(Some(port));
        }
    }
    Ok(None)
}
