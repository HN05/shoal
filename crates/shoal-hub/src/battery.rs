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

/// IOKit's power-source query, with IOKit and Core Foundation loaded at run
/// time, so that the binary links only libSystem: the release cross-builds
/// macOS against an SDK without Apple frameworks.
#[cfg(target_os = "macos")]
mod iokit {
    use std::ffi::{CStr, c_char, c_void};

    type CFTypeRef = *const c_void;
    type CFIndex = isize;

    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;

    const IOKIT: &CStr = c"/System/Library/Frameworks/IOKit.framework/IOKit";
    const CORE_FOUNDATION: &CStr =
        c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation";

    /// The IOKit and Core Foundation functions the query calls.
    struct Functions {
        copy_power_sources_info: unsafe extern "C" fn() -> CFTypeRef,
        copy_power_sources_list: unsafe extern "C" fn(blob: CFTypeRef) -> CFTypeRef,
        get_power_source_description:
            unsafe extern "C" fn(blob: CFTypeRef, source: CFTypeRef) -> CFTypeRef,
        array_get_count: unsafe extern "C" fn(array: CFTypeRef) -> CFIndex,
        array_get_value_at_index:
            unsafe extern "C" fn(array: CFTypeRef, index: CFIndex) -> CFTypeRef,
        dictionary_get_value:
            unsafe extern "C" fn(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef,
        string_create_with_c_string: unsafe extern "C" fn(
            allocator: CFTypeRef,
            text: *const c_char,
            encoding: u32,
        ) -> CFTypeRef,
        equal: unsafe extern "C" fn(one: CFTypeRef, other: CFTypeRef) -> u8,
        release: unsafe extern "C" fn(object: CFTypeRef),
    }

    impl Functions {
        /// The functions, none when a framework or one of its symbols fails
        /// to load.
        fn load() -> Option<Self> {
            let iokit = Library::open(IOKIT)?;
            let core_foundation = Library::open(CORE_FOUNDATION)?;
            // SAFETY: each type is the C signature of the symbol it names.
            unsafe {
                Some(Self {
                    copy_power_sources_info: iokit.function(c"IOPSCopyPowerSourcesInfo")?,
                    copy_power_sources_list: iokit.function(c"IOPSCopyPowerSourcesList")?,
                    get_power_source_description: iokit
                        .function(c"IOPSGetPowerSourceDescription")?,
                    array_get_count: core_foundation.function(c"CFArrayGetCount")?,
                    array_get_value_at_index: core_foundation
                        .function(c"CFArrayGetValueAtIndex")?,
                    dictionary_get_value: core_foundation.function(c"CFDictionaryGetValue")?,
                    string_create_with_c_string: core_foundation
                        .function(c"CFStringCreateWithCString")?,
                    equal: core_foundation.function(c"CFEqual")?,
                    release: core_foundation.function(c"CFRelease")?,
                })
            }
        }
    }

    /// A framework `dlopen` loaded, never closed, so that the functions read
    /// from it stay valid.
    struct Library(*mut c_void);

    impl Library {
        fn open(path: &CStr) -> Option<Self> {
            // SAFETY: `path` is NUL-terminated.
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
            (!handle.is_null()).then_some(Self(handle))
        }

        /// The function `name` names, none when the library lacks it.
        ///
        /// # Safety
        ///
        /// `F` is a function pointer type matching the symbol's signature.
        unsafe fn function<F: Copy>(&self, name: &CStr) -> Option<F> {
            const {
                assert!(size_of::<F>() == size_of::<*mut c_void>());
            }
            // SAFETY: the handle came from `dlopen`; `name` is NUL-terminated.
            let symbol = unsafe { libc::dlsym(self.0, name.as_ptr()) };
            // SAFETY: the caller guarantees `F` is the symbol's function type,
            // which has a pointer's size.
            (!symbol.is_null()).then(|| unsafe { std::mem::transmute_copy(&symbol) })
        }
    }

    /// A Core Foundation object this code owns, released when dropped.
    struct Owned<'a> {
        object: CFTypeRef,
        functions: &'a Functions,
    }

    impl<'a> Owned<'a> {
        /// `object`, from a Copy or Create function, none for null.
        fn new(functions: &'a Functions, object: CFTypeRef) -> Option<Self> {
            (!object.is_null()).then_some(Self { object, functions })
        }

        fn string(functions: &'a Functions, text: &CStr) -> Option<Self> {
            // SAFETY: `text` is NUL-terminated; a null allocator is the default.
            let object = unsafe {
                (functions.string_create_with_c_string)(std::ptr::null(), text.as_ptr(), UTF8)
            };
            Self::new(functions, object)
        }
    }

    impl Drop for Owned<'_> {
        fn drop(&mut self) {
            // SAFETY: the object came from a Copy or Create function and is
            // released once.
            unsafe { (self.functions.release)(self.object) }
        }
    }

    /// Whether a power source's `Type` is `InternalBattery`, as
    /// `kIOPSTypeKey` and `kIOPSInternalBatteryType` spell them; false when
    /// IOKit or Core Foundation fails to load.
    pub(super) fn internal_battery() -> bool {
        Functions::load().is_some_and(|functions| lists_internal_battery(&functions))
    }

    fn lists_internal_battery(functions: &Functions) -> bool {
        let (Some(key), Some(battery)) = (
            Owned::string(functions, c"Type"),
            Owned::string(functions, c"InternalBattery"),
        ) else {
            return false;
        };
        // SAFETY: each Copy result is owned and released; the description
        // and the values read from it belong to the blob, which outlives them.
        unsafe {
            let Some(blob) = Owned::new(functions, (functions.copy_power_sources_info)()) else {
                return false;
            };
            let Some(sources) =
                Owned::new(functions, (functions.copy_power_sources_list)(blob.object))
            else {
                return false;
            };
            (0..(functions.array_get_count)(sources.object)).any(|index| {
                let source = (functions.array_get_value_at_index)(sources.object, index);
                let description = (functions.get_power_source_description)(blob.object, source);
                if description.is_null() {
                    return false;
                }
                let kind = (functions.dictionary_get_value)(description, key.object);
                !kind.is_null() && (functions.equal)(kind, battery.object) != 0
            })
        }
    }
}

#[cfg(test)]
mod tests;
