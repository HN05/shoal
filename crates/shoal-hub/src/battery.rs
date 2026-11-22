//! Whether this machine has a battery, which tells the hub a new machine
//! may be portable, and how it is powered now, which the hub reads a drop
//! by: an internal battery among IOKit's power sources on macOS, a
//! `power_supply` entry of type `Battery` that powers the system, not a
//! device such as a mouse, on Linux.
use std::sync::OnceLock;

use shoal_protocol::hub::sleep::Power;

/// Whether this machine has a battery, read once.
pub(crate) fn present() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(read)
}

/// How this machine is powered now, none without a battery.
pub(crate) fn power() -> Option<Power> {
    present().then(read_power).flatten()
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

#[cfg(target_os = "macos")]
fn read_power() -> Option<Power> {
    iokit::power()
}

#[cfg(target_os = "linux")]
fn read_power() -> Option<Power> {
    system_battery_power(std::path::Path::new("/sys/class/power_supply"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_power() -> Option<Power> {
    None
}

/// The batteries under `class`, a `power_supply` class directory, that
/// power the system: of type `Battery`, with no `scope` or one other than
/// `Device`.
#[cfg(any(target_os = "linux", test))]
fn system_batteries(class: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(supplies) = std::fs::read_dir(class) else {
        return Vec::new();
    };
    (supplies.flatten())
        .map(|supply| supply.path())
        .filter(|supply| {
            attribute(supply, "type").as_deref() == Some("Battery")
                && attribute(supply, "scope").as_deref() != Some("Device")
        })
        .collect()
}

/// The attribute `name` of the supply `supply`, trimmed.
#[cfg(any(target_os = "linux", test))]
fn attribute(supply: &std::path::Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(supply.join(name)).ok()?;
    Some(text.trim().to_owned())
}

/// Whether a supply under `class` is a battery that powers the system.
#[cfg(any(target_os = "linux", test))]
fn powers_system_from_battery(class: &std::path::Path) -> bool {
    !system_batteries(class).is_empty()
}

/// How the system batteries under `class` power it: on battery while one
/// discharges, at their mean level; none without one that says its level.
#[cfg(any(target_os = "linux", test))]
fn system_battery_power(class: &std::path::Path) -> Option<Power> {
    let batteries = system_batteries(class);
    let levels: Vec<u32> = (batteries.iter())
        .filter_map(|battery| attribute(battery, "capacity")?.parse().ok())
        .collect();
    let mean = levels
        .iter()
        .sum::<u32>()
        .checked_div(levels.len() as u32)?;
    let on_battery = (batteries.iter())
        .any(|battery| attribute(battery, "status").as_deref() == Some("Discharging"));
    Some(Power {
        on_battery,
        percent: mean.min(100) as u8,
    })
}

/// IOKit's power-source query, with IOKit and Core Foundation loaded at run
/// time.
#[cfg(target_os = "macos")]
mod iokit {
    use std::ffi::{CStr, c_char, c_void};

    use shoal_daemon_base::frameworks::Library;

    type CFTypeRef = *const c_void;
    type CFIndex = isize;

    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;

    /// `kCFNumberSInt32Type`.
    const SINT32: CFIndex = 3;

    const IOKIT: &CStr = c"/System/Library/Frameworks/IOKit.framework/IOKit";
    const CORE_FOUNDATION: &CStr =
        c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation";

    /// The IOKit and Core Foundation functions the query calls.
    struct Functions {
        copy_power_sources_info: unsafe extern "C" fn() -> CFTypeRef,
        copy_power_sources_list: unsafe extern "C" fn(blob: CFTypeRef) -> CFTypeRef,
        get_power_source_description:
            unsafe extern "C" fn(blob: CFTypeRef, source: CFTypeRef) -> CFTypeRef,
        get_providing_power_source_type: unsafe extern "C" fn(blob: CFTypeRef) -> CFTypeRef,
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
        number_get_value:
            unsafe extern "C" fn(number: CFTypeRef, kind: CFIndex, value: *mut c_void) -> u8,
        release: unsafe extern "C" fn(object: CFTypeRef),
    }

    impl Functions {
        /// The functions, loaded once, none when a framework or one of its
        /// symbols failed to load.
        fn loaded() -> Option<&'static Self> {
            static LOADED: std::sync::OnceLock<Option<Functions>> = std::sync::OnceLock::new();
            LOADED.get_or_init(Self::load).as_ref()
        }

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
                    get_providing_power_source_type: iokit
                        .function(c"IOPSGetProvidingPowerSourceType")?,
                    array_get_count: core_foundation.function(c"CFArrayGetCount")?,
                    array_get_value_at_index: core_foundation
                        .function(c"CFArrayGetValueAtIndex")?,
                    dictionary_get_value: core_foundation.function(c"CFDictionaryGetValue")?,
                    string_create_with_c_string: core_foundation
                        .function(c"CFStringCreateWithCString")?,
                    equal: core_foundation.function(c"CFEqual")?,
                    number_get_value: core_foundation.function(c"CFNumberGetValue")?,
                    release: core_foundation.function(c"CFRelease")?,
                })
            }
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
        Functions::loaded()
            .is_some_and(|functions| with_internal_battery(functions, |_, _| Some(())).is_some())
    }

    /// How the internal battery powers the machine: on battery while
    /// battery power provides it, as `kIOPSBatteryPowerValue` spells it,
    /// at the battery's `Current Capacity` of its `Max Capacity`; none
    /// when IOKit or Core Foundation fails to load or the battery does
    /// not say.
    pub(super) fn power() -> Option<super::Power> {
        let functions = Functions::loaded()?;
        let battery_power = Owned::string(functions, c"Battery Power")?;
        let (current, max) = (
            Owned::string(functions, c"Current Capacity")?,
            Owned::string(functions, c"Max Capacity")?,
        );
        with_internal_battery(functions, |blob, description| {
            let number = |key: &Owned<'_>| {
                let mut value: i32 = 0;
                // SAFETY: the value belongs to the description, which
                // outlives this read; `value` holds an SInt32.
                let read = unsafe {
                    let number = (functions.dictionary_get_value)(description, key.object);
                    !number.is_null()
                        && (functions.number_get_value)(number, SINT32, (&raw mut value).cast())
                            != 0
                };
                read.then_some(value)
            };
            let (current, max) = (number(&current)?, number(&max)?);
            let percent = (i64::from(current) * 100).checked_div(i64::from(max))?;
            // SAFETY: the type belongs to the blob, which outlives this read.
            let on_battery = unsafe {
                let providing = (functions.get_providing_power_source_type)(blob);
                !providing.is_null() && (functions.equal)(providing, battery_power.object) != 0
            };
            Some(super::Power {
                on_battery,
                percent: percent.clamp(0, 100) as u8,
            })
        })
    }

    /// What `read` makes of the power sources' blob and the description of
    /// the first internal battery among them, none without one or when
    /// `read` makes nothing of it.
    fn with_internal_battery<T>(
        functions: &Functions,
        read: impl Fn(CFTypeRef, CFTypeRef) -> Option<T>,
    ) -> Option<T> {
        let key = Owned::string(functions, c"Type")?;
        let battery = Owned::string(functions, c"InternalBattery")?;
        // SAFETY: each Copy result is owned and released; the description
        // and the values read from it belong to the blob, which outlives them.
        unsafe {
            let blob = Owned::new(functions, (functions.copy_power_sources_info)())?;
            let sources = Owned::new(functions, (functions.copy_power_sources_list)(blob.object))?;
            (0..(functions.array_get_count)(sources.object)).find_map(|index| {
                let source = (functions.array_get_value_at_index)(sources.object, index);
                let description = (functions.get_power_source_description)(blob.object, source);
                if description.is_null() {
                    return None;
                }
                let kind = (functions.dictionary_get_value)(description, key.object);
                let internal = !kind.is_null() && (functions.equal)(kind, battery.object) != 0;
                internal.then(|| read(blob.object, description)).flatten()
            })
        }
    }
}

#[cfg(test)]
mod tests;
