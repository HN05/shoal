//! Whether this machine has a battery, which tells the hub a new machine
//! may be portable: an internal battery among IOKit's power sources on
//! macOS, a `power_supply` entry of type `Battery` that powers the system,
//! not a device such as a mouse, on Linux.
use std::sync::OnceLock;

/// Whether this machine has a battery, read once.
pub(crate) fn present() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(read)
}

#[cfg(target_os = "macos")]
fn read() -> bool {
    iokit::internal_battery()
}

#[cfg(target_os = "linux")]
fn read() -> bool {
    powers_system_from_battery(std::path::Path::new("/sys/class/power_supply"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read() -> bool {
    false
}

/// Whether a supply under `class`, a `power_supply` class directory, is a
/// battery that powers the system: of type `Battery`, with no `scope` or one
/// other than `Device`.
#[cfg(any(target_os = "linux", test))]
fn powers_system_from_battery(class: &std::path::Path) -> bool {
    let Ok(supplies) = std::fs::read_dir(class) else {
        return false;
    };
    let attribute = |supply: &std::path::Path, name: &str| {
        let text = std::fs::read_to_string(supply.join(name)).ok()?;
        Some(text.trim().to_owned())
    };
    supplies.flatten().any(|supply| {
        let supply = supply.path();
        attribute(&supply, "type").as_deref() == Some("Battery")
            && attribute(&supply, "scope").as_deref() != Some("Device")
    })
}

#[cfg(target_os = "macos")]
mod iokit {
    use std::ffi::{c_char, c_void};

    type CFTypeRef = *const c_void;
    type CFIndex = isize;

    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
        fn IOPSCopyPowerSourcesList(blob: CFTypeRef) -> CFTypeRef;
        fn IOPSGetPowerSourceDescription(blob: CFTypeRef, source: CFTypeRef) -> CFTypeRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: CFTypeRef) -> CFIndex;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, index: CFIndex) -> CFTypeRef;
        fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFStringCreateWithCString(
            allocator: CFTypeRef,
            text: *const c_char,
            encoding: u32,
        ) -> CFTypeRef;
        fn CFEqual(one: CFTypeRef, other: CFTypeRef) -> u8;
        fn CFRelease(object: CFTypeRef);
    }

    /// A Core Foundation object this code owns, released when dropped.
    struct Owned(CFTypeRef);

    impl Owned {
        /// `object`, from a Copy or Create function, none for null.
        fn new(object: CFTypeRef) -> Option<Self> {
            (!object.is_null()).then_some(Self(object))
        }

        fn string(text: &std::ffi::CStr) -> Option<Self> {
            // SAFETY: `text` is NUL-terminated; a null allocator is the default.
            Self::new(unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), UTF8) })
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the object came from a Copy or Create function and is
            // released once.
            unsafe { CFRelease(self.0) }
        }
    }

    /// Whether a power source's `Type` is `InternalBattery`, as
    /// `kIOPSTypeKey` and `kIOPSInternalBatteryType` spell them.
    pub(super) fn internal_battery() -> bool {
        let (Some(key), Some(battery)) =
            (Owned::string(c"Type"), Owned::string(c"InternalBattery"))
        else {
            return false;
        };
        // SAFETY: each Copy result is owned and released; the description
        // and the values read from it belong to the blob, which outlives them.
        unsafe {
            let Some(blob) = Owned::new(IOPSCopyPowerSourcesInfo()) else {
                return false;
            };
            let Some(sources) = Owned::new(IOPSCopyPowerSourcesList(blob.0)) else {
                return false;
            };
            (0..CFArrayGetCount(sources.0)).any(|index| {
                let source = CFArrayGetValueAtIndex(sources.0, index);
                let description = IOPSGetPowerSourceDescription(blob.0, source);
                if description.is_null() {
                    return false;
                }
                let kind = CFDictionaryGetValue(description, key.0);
                !kind.is_null() && CFEqual(kind, battery.0) != 0
            })
        }
    }
}

#[cfg(test)]
mod tests;
