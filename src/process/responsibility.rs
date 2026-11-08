//! Which process macOS asks for privacy grants, such as access to a removable
//! volume. macOS asks the process responsible for the one making the access:
//! by default the app or launchd job that started it, inherited by everything
//! started below. A host that daemonizes, such as zmx, leaves what it runs with
//! a responsible process that has exited, and macOS then judges each new
//! program by itself, so a restarted agent asks for Claude Code's own grant.
//! An execution wrapper outlives what it starts, so it claims responsibility
//! and everything it runs uses Shoal's grant.
use crate::releases::Releases;

/// Make this process responsible for itself and everything it starts, by
/// replacing it with a copy of its release in `releases`, or with its own
/// binary when the copy fails, started without its starter's responsibility.
/// Grants for an ad-hoc signed build last only as long as that build, so such
/// builds keep their starter's. Only for processes that never replace
/// themselves with another program, which would take over the responsibility
/// and ask for its own grants.
///
/// # Safety
/// Call before any other thread exists.
pub unsafe fn claim(releases: Option<Releases>) {
    #[cfg(target_os = "macos")]
    // SAFETY: the caller guarantees no other thread exists.
    unsafe {
        macos::claim(releases.as_ref())
    }
    #[cfg(not(target_os = "macos"))]
    let _ = releases;
}

#[cfg(target_os = "macos")]
mod macos {
    use std::{
        ffi::{CString, c_char, c_int},
        io,
        os::unix::ffi::OsStringExt,
        ptr,
    };

    use crate::{
        env,
        releases::{Invoked, Releases, Replacement},
    };

    pub(super) unsafe fn claim(releases: Option<&Releases>) {
        // SAFETY: the caller guarantees no other thread reads the environment.
        if unsafe { take_claimed() }
            || !crate::process::code_signing::own_signature_has_certificate()
        {
            return;
        }
        let error = replace(releases);
        eprintln!("warning: macOS privacy access follows the app that started Shoal: {error}");
    }

    /// Replace this process with a copy of its release, responsible for
    /// itself. Returns only on failure.
    fn replace(releases: Option<&Releases>) -> io::Error {
        let replacement = crate::fsutil::executables::running_executable().and_then(|executable| {
            let invoked = Invoked::at(crate::fsutil::executables::invoked_executable()?);
            Replacement::of(releases, executable, &invoked)
        });
        // The replacement holds the copy's lock open until the exec.
        let replacement = match replacement {
            Ok(replacement) => replacement,
            Err(error) => return error,
        };
        match Image::of(&replacement) {
            Ok(image) => image.replace_disclaimed(),
            Err(error) => error,
        }
    }

    unsafe extern "C" {
        fn responsibility_spawnattrs_setdisclaim(
            attrs: *mut libc::posix_spawnattr_t,
            disclaim: c_int,
        ) -> c_int;
    }

    /// Whether this process is the image an earlier claim replaced it with;
    /// the marker goes so that what it starts claims again where it must.
    ///
    /// # Safety
    /// Call before any other thread exists.
    unsafe fn take_claimed() -> bool {
        let claimed = std::env::var_os(env::RESPONSIBILITY_CLAIMED).is_some();
        // SAFETY: the caller guarantees no other thread reads the environment.
        unsafe { std::env::remove_var(env::RESPONSIBILITY_CLAIMED) };
        claimed
    }

    /// The replacement's program with this process's arguments and
    /// environment, the replacement's variables, marked as claimed.
    pub(super) struct Image {
        pub(super) program: CString,
        pub(super) args: Vec<CString>,
        pub(super) vars: Vec<CString>,
    }

    impl Image {
        pub(super) fn of(replacement: &Replacement) -> io::Result<Self> {
            let program = CString::new(replacement.program.clone().into_os_string().into_vec())?;
            let args = std::env::args_os()
                .map(|arg| CString::new(arg.into_vec()))
                .collect::<Result<_, _>>()?;
            let handed = replacement
                .variables
                .iter()
                .map(|(name, value)| (name.into(), value.clone()));
            let vars = std::env::vars_os()
                .chain(handed)
                .map(|(name, value): (std::ffi::OsString, std::ffi::OsString)| {
                    let mut var = name.into_vec();
                    var.push(b'=');
                    var.extend(value.into_vec());
                    var
                })
                .chain([format!("{}=1", env::RESPONSIBILITY_CLAIMED).into_bytes()])
                .map(CString::new)
                .collect::<Result<_, _>>()?;
            Ok(Self {
                program,
                args,
                vars,
            })
        }

        /// Replace this process with the image, responsible for itself.
        /// Returns only on failure.
        fn replace_disclaimed(&self) -> io::Error {
            match Attributes::disclaiming(libc::POSIX_SPAWN_SETEXEC)
                .and_then(|attributes| attributes.spawn(self))
            {
                Ok(_) => io::Error::other("posix_spawn returned instead of replacing Shoal"),
                Err(error) => error,
            }
        }
    }

    /// Spawn attributes that start a process responsible for itself.
    pub(super) struct Attributes(libc::posix_spawnattr_t);

    impl Attributes {
        pub(super) fn disclaiming(flags: c_int) -> io::Result<Self> {
            let mut attributes = ptr::null_mut();
            // SAFETY: posix_spawnattr_init initializes the attributes it is given.
            check(unsafe { libc::posix_spawnattr_init(&mut attributes) })?;
            let mut attributes = Self(attributes);
            let flags = libc::c_short::try_from(flags).map_err(io::Error::other)?;
            // SAFETY: the attributes were initialized above.
            check(unsafe { libc::posix_spawnattr_setflags(&mut attributes.0, flags) })?;
            // SAFETY: as above.
            check(unsafe { responsibility_spawnattrs_setdisclaim(&mut attributes.0, 1) })?;
            Ok(attributes)
        }

        /// Start `image` with these attributes, returning its process ID.
        pub(super) fn spawn(&self, image: &Image) -> io::Result<libc::pid_t> {
            let args = null_terminated(&image.args);
            let vars = null_terminated(&image.vars);
            let mut pid = 0;
            // SAFETY: the program, arguments and environment are NUL-terminated
            // strings, the lists end with a null pointer, and all outlive the call.
            check(unsafe {
                libc::posix_spawn(
                    &mut pid,
                    image.program.as_ptr(),
                    ptr::null(),
                    &self.0,
                    args.as_ptr().cast(),
                    vars.as_ptr().cast(),
                )
            })?;
            Ok(pid)
        }
    }

    impl Drop for Attributes {
        fn drop(&mut self) {
            // SAFETY: the attributes were initialized and are not used after this.
            unsafe { libc::posix_spawnattr_destroy(&mut self.0) };
        }
    }

    fn null_terminated(strings: &[CString]) -> Vec<*const c_char> {
        strings
            .iter()
            .map(|string| string.as_ptr())
            .chain([ptr::null()])
            .collect()
    }

    /// posix_spawn functions return their error number instead of setting errno.
    fn check(status: c_int) -> io::Result<()> {
        match status {
            0 => Ok(()),
            error => Err(io::Error::from_raw_os_error(error)),
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
