//! What a process's code signature says, read from the kernel with `csops`.
use std::ffi::c_void;

unsafe extern "C" {
    fn csops(pid: libc::pid_t, ops: u32, useraddr: *mut c_void, usersize: usize) -> libc::c_int;
}

/// `CS_OPS_*` operations from `sys/codesign.h`.
const CS_OPS_STATUS: u32 = 0;
const CS_OPS_BLOB: u32 = 10;

/// The code signing status flags of process `pid`, from `sys/codesign.h`.
pub(super) fn status_flags(pid: libc::pid_t) -> Option<u32> {
    let mut flags = 0_u32;
    // SAFETY: CS_OPS_STATUS writes the 32-bit status flags into `flags`.
    let result = unsafe {
        csops(
            pid,
            CS_OPS_STATUS,
            (&raw mut flags).cast(),
            size_of::<u32>(),
        )
    };
    (result == 0).then_some(flags)
}

/// Whether a certificate signs this process's binary. The kernel's status
/// flags cannot tell: they mark neither linker-signed nor `codesign -s -`
/// binaries as ad hoc.
pub(super) fn own_signature_has_certificate() -> bool {
    own_signature().is_some_and(|signature| carries_certificate(&signature))
}

/// This process's code signature, a superblob of big-endian blobs.
fn own_signature() -> Option<Vec<u8>> {
    let pid = std::process::id() as libc::pid_t;
    let mut header = [0_u8; 8];
    // SAFETY: CS_OPS_BLOB writes at most `header.len()` bytes; a larger
    // signature fails with ERANGE after writing its header, whose second word
    // is the signature's length.
    unsafe { csops(pid, CS_OPS_BLOB, header.as_mut_ptr().cast(), header.len()) };
    let length = usize::try_from(read_u32(&header, 4)?).ok()?;
    if length <= header.len() {
        return None;
    }
    let mut signature = vec![0_u8; length];
    // SAFETY: as above, with a buffer that holds the whole signature.
    let result = unsafe { csops(pid, CS_OPS_BLOB, signature.as_mut_ptr().cast(), length) };
    (result == 0).then_some(signature)
}

/// Whether `signature` holds a CMS signature. Ad-hoc signatures have no
/// signature blob, or one with no content.
pub(super) fn carries_certificate(signature: &[u8]) -> bool {
    const SUPERBLOB: u32 = 0xfade_0cc0;
    const SIGNATURE_SLOT: u32 = 0x1_0000;
    const WRAPPER: u32 = 0xfade_0b01;
    const HEADER: u32 = 8;
    if read_u32(signature, 0) != Some(SUPERBLOB) {
        return false;
    }
    let count = read_u32(signature, 8).unwrap_or(0);
    (0..count).any(|index| {
        let entry = 12 + 8 * index as usize;
        read_u32(signature, entry) == Some(SIGNATURE_SLOT)
            && read_u32(signature, entry + 4).is_some_and(|offset| {
                let offset = offset as usize;
                read_u32(signature, offset) == Some(WRAPPER)
                    && read_u32(signature, offset + 4).is_some_and(|length| length > HEADER)
            })
    })
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let word = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes(word.try_into().ok()?))
}

#[cfg(test)]
mod tests;
