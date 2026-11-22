//! Apple frameworks loaded at run time, so that the binary links only
//! libSystem: the release cross-builds macOS against an SDK without Apple
//! frameworks.
use std::ffi::{CStr, c_void};

/// A framework `dlopen` loaded, never closed, so that the functions and
/// values read from it stay valid.
pub struct Library(*mut c_void);

impl Library {
    /// The framework at `path`, none when it does not load.
    pub fn open(path: &CStr) -> Option<Self> {
        // SAFETY: `path` is NUL-terminated.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        (!handle.is_null()).then_some(Self(handle))
    }

    /// The address of the symbol `name` names, none when the library lacks
    /// it.
    pub fn symbol(&self, name: &CStr) -> Option<*mut c_void> {
        // SAFETY: the handle came from `dlopen`; `name` is NUL-terminated.
        let symbol = unsafe { libc::dlsym(self.0, name.as_ptr()) };
        (!symbol.is_null()).then_some(symbol)
    }

    /// The function `name` names, none when the library lacks it.
    ///
    /// # Safety
    ///
    /// `F` is a function pointer type matching the symbol's signature.
    pub unsafe fn function<F: Copy>(&self, name: &CStr) -> Option<F> {
        const {
            assert!(size_of::<F>() == size_of::<*mut c_void>());
        }
        let symbol = self.symbol(name)?;
        // SAFETY: the caller guarantees `F` is the symbol's function type,
        // which has a pointer's size.
        Some(unsafe { std::mem::transmute_copy(&symbol) })
    }
}
