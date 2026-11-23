//! IOKit's system power notifications, with IOKit and Core Foundation
//! loaded at run time, as the release cross-builds macOS against an SDK
//! without Apple frameworks. A thread of its own runs the run loop the
//! notifications arrive on. macOS waits for a will-sleep notification's
//! acknowledgement up to [`ALLOWANCE`]; it lets an idle sleep it asks about
//! go at once, and tells a client registered this way only of full wakes,
//! so a dark wake passes unseen.
use std::{
    ffi::{CStr, c_void},
    time::Duration,
};

use shoal_daemon_base::frameworks::Library;
use tokio::sync::mpsc;

use super::{PowerEvent, PowerEvents};
use crate::daemon::log;

/// How long macOS waits for a will-sleep notification's acknowledgement.
const ALLOWANCE: Duration = Duration::from_secs(30);

/// `kIOMessageCanSystemSleep`: an idle sleep asks whether it may go.
const CAN_SYSTEM_SLEEP: u32 = 0xe000_0270;
/// `kIOMessageSystemWillSleep`: the machine goes to sleep once acknowledged.
const SYSTEM_WILL_SLEEP: u32 = 0xe000_0280;
/// `kIOMessageSystemHasPoweredOn`: the machine fully woke.
const SYSTEM_HAS_POWERED_ON: u32 = 0xe000_0300;

const IOKIT: &CStr = c"/System/Library/Frameworks/IOKit.framework/IOKit";
const CORE_FOUNDATION: &CStr =
    c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation";

type Callback =
    unsafe extern "C" fn(refcon: *mut c_void, service: u32, message: u32, argument: *mut c_void);

/// The IOKit and Core Foundation functions the notifications take.
struct Functions {
    register_for_system_power: unsafe extern "C" fn(
        refcon: *mut c_void,
        port: *mut *mut c_void,
        callback: Callback,
        notifier: *mut u32,
    ) -> u32,
    notification_port_get_run_loop_source: unsafe extern "C" fn(port: *mut c_void) -> *mut c_void,
    allow_power_change: unsafe extern "C" fn(kernel_port: u32, notification: isize) -> i32,
    run_loop_get_current: unsafe extern "C" fn() -> *mut c_void,
    run_loop_add_source:
        unsafe extern "C" fn(run_loop: *mut c_void, source: *mut c_void, mode: *const c_void),
    run_loop_run: unsafe extern "C" fn(),
}

/// The functions with the run loop's default mode, none when a framework
/// or one of its symbols fails to load.
fn load() -> Option<(Functions, *const c_void)> {
    let iokit = Library::open(IOKIT)?;
    let core_foundation = Library::open(CORE_FOUNDATION)?;
    // SAFETY: each type is the C signature of the symbol it names, and
    // `kCFRunLoopDefaultMode` is a `CFStringRef` constant.
    unsafe {
        let functions = Functions {
            register_for_system_power: iokit.function(c"IORegisterForSystemPower")?,
            notification_port_get_run_loop_source: iokit
                .function(c"IONotificationPortGetRunLoopSource")?,
            allow_power_change: iokit.function(c"IOAllowPowerChange")?,
            run_loop_get_current: core_foundation.function(c"CFRunLoopGetCurrent")?,
            run_loop_add_source: core_foundation.function(c"CFRunLoopAddSource")?,
            run_loop_run: core_foundation.function(c"CFRunLoopRun")?,
        };
        let mode = core_foundation.symbol(c"kCFRunLoopDefaultMode")?;
        Some((functions, *mode.cast::<*const c_void>()))
    }
}

/// What the callback reaches: the root power domain's port, which
/// acknowledges notifications, and where announcements go.
struct Context {
    functions: &'static Functions,
    port: u32,
    events: mpsc::UnboundedSender<PowerEvent<Acknowledgement>>,
}

/// A will-sleep notification's acknowledgement, which lets the sleep go
/// once dropped.
pub(super) struct Acknowledgement {
    allow: unsafe extern "C" fn(kernel_port: u32, notification: isize) -> i32,
    port: u32,
    notification: isize,
}

impl Drop for Acknowledgement {
    fn drop(&mut self) {
        // SAFETY: the port and notification came from IOKit's callback.
        unsafe { (self.allow)(self.port, self.notification) };
    }
}

pub(super) struct IoKit {
    events: mpsc::UnboundedReceiver<PowerEvent<Acknowledgement>>,
}

/// IOKit's notifications, once their thread registered for them.
pub(super) async fn connect() -> Option<IoKit> {
    let (sender, events) = mpsc::unbounded_channel();
    let (registered, answer) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("sleep lock".into())
        .spawn(move || listen(sender, registered));
    if let Err(error) = spawned {
        log!("sleep lock: could not start its thread: {error}");
        return None;
    }
    match answer.await {
        Ok(Ok(())) => Some(IoKit { events }),
        Ok(Err(error)) => {
            log!("sleep lock: {error}");
            None
        }
        Err(_) => None,
    }
}

/// Register for the notifications and run their run loop for as long as
/// the daemon runs, saying on `registered` whether the registration took.
fn listen(
    events: mpsc::UnboundedSender<PowerEvent<Acknowledgement>>,
    registered: tokio::sync::oneshot::Sender<Result<(), &'static str>>,
) {
    let Some((functions, mode)) = load() else {
        let _ = registered.send(Err("IOKit or Core Foundation did not load"));
        return;
    };
    // Lives as long as the run loop, which never returns.
    let functions: &'static Functions = Box::leak(Box::new(functions));
    // One pointer, which the callback and this thread both go through.
    let context = Box::into_raw(Box::new(Context {
        functions,
        port: 0,
        events,
    }));
    let mut notify_port = std::ptr::null_mut();
    let mut notifier = 0;
    // SAFETY: the context outlives the registration, and the out-pointers
    // are valid for writes.
    let port = unsafe {
        (functions.register_for_system_power)(
            context.cast(),
            &raw mut notify_port,
            notified,
            &raw mut notifier,
        )
    };
    if port == 0 || notify_port.is_null() {
        let _ = registered.send(Err("IOKit refused the system power registration"));
        return;
    }
    // SAFETY: the context is never freed, and the callback reads it only
    // from the run loop, which runs once this write is done.
    unsafe { (*context).port = port };
    let _ = registered.send(Ok(()));
    // SAFETY: the port came from the registration, and this thread's run
    // loop runs the source it gives.
    unsafe {
        let source = (functions.notification_port_get_run_loop_source)(notify_port);
        (functions.run_loop_add_source)((functions.run_loop_get_current)(), source, mode);
        (functions.run_loop_run)();
    }
}

/// IOKit's callback: let an idle sleep go, hold a coming sleep for the
/// daemon, and pass on a full wake.
unsafe extern "C" fn notified(refcon: *mut c_void, _: u32, message: u32, argument: *mut c_void) {
    // SAFETY: the registration passed the leaked context as `refcon`.
    let context = unsafe { &*refcon.cast::<Context>() };
    let acknowledgement = || Acknowledgement {
        allow: context.functions.allow_power_change,
        port: context.port,
        notification: argument as isize,
    };
    let event = match message {
        CAN_SYSTEM_SLEEP => return drop(acknowledgement()),
        SYSTEM_WILL_SLEEP => PowerEvent::Sleeping {
            hold: Some(acknowledgement()),
            allowance: ALLOWANCE,
        },
        SYSTEM_HAS_POWERED_ON => PowerEvent::Woke,
        _ => return,
    };
    // Once the daemon stops listening, a hold dropped here lets the sleep go.
    let _ = context.events.send(event);
}

impl PowerEvents for IoKit {
    type Hold = Acknowledgement;

    async fn next(&mut self) -> Option<PowerEvent<Acknowledgement>> {
        self.events.recv().await
    }
}
