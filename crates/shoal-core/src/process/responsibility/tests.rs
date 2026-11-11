use std::{
    ffi::{CString, c_int},
    os::unix::ffi::OsStringExt,
    time::{Duration, Instant},
};

use super::macos::{Attributes, Image};
use crate::{
    env,
    releases::{Invoked, Replacement},
};

unsafe extern "C" {
    fn responsibility_get_pid_responsible_for_pid(pid: libc::pid_t) -> libc::pid_t;
}

#[test]
fn image_runs_the_replacement_with_its_variables_marked_as_claimed() {
    let program = std::env::current_exe().unwrap();
    let invoked = Invoked::at(program.clone());
    let replacement = Replacement::of(None, program.clone(), &invoked).unwrap();
    let image = Image::of(&replacement).unwrap();

    assert_eq!(
        image.program.as_bytes(),
        program.into_os_string().into_vec()
    );
    let args: Vec<_> = std::env::args_os().map(OsStringExt::into_vec).collect();
    let image_args: Vec<_> = image
        .args
        .iter()
        .map(|arg| arg.as_bytes().to_vec())
        .collect();
    assert_eq!(image_args, args);
    let marker = format!("{}=1", env::RESPONSIBILITY_CLAIMED);
    assert_eq!(image.vars.last().unwrap().to_str().unwrap(), marker);
    let handed = format!("{}=", env::RELEASE_INVOKED);
    assert!(
        image
            .vars
            .iter()
            .any(|var| var.to_bytes().starts_with(handed.as_bytes())),
        "the replacement's variables are handed on"
    );
    assert!(image.vars.len() > 1, "the environment is kept");
}

#[test]
fn disclaimed_process_is_responsible_for_what_it_starts() {
    // The inner shell stops itself, so both stay until `end` kills them.
    let shell = Image {
        program: CString::new("/bin/sh").unwrap(),
        args: ["sh", "-c", "/bin/sh -c 'kill -STOP $$'; :"]
            .map(|arg| CString::new(arg).unwrap())
            .into(),
        vars: vec![],
    };
    let pid = Attributes::disclaiming(0).unwrap().spawn(&shell).unwrap();
    let child = child_of(pid);
    // SAFETY: the lookup only reads the kernel's record of `child`.
    let responsible =
        child.map(|child| unsafe { responsibility_get_pid_responsible_for_pid(child) });
    end(child.into_iter().chain([pid]));

    assert_eq!(responsible, Some(pid));
}

/// The first process `parent` starts, waiting as long as a slow machine could need.
fn child_of(parent: libc::pid_t) -> Option<libc::pid_t> {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        let output = std::process::Command::new("pgrep")
            .args(["-P", &parent.to_string()])
            .output()
            .ok()?;
        let found = String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.trim().parse().ok());
        if found.is_some() {
            return found;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// Kill the test's processes, children first so none outlives its parent,
/// and reap the outer shell. SIGKILL ends a stopped process too.
fn end(pids: impl IntoIterator<Item = libc::pid_t>) {
    let mut status: c_int = 0;
    for pid in pids {
        // SAFETY: each pid is a process this test started that has not been
        // reaped, so it names no other process.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, &mut status, 0);
        }
    }
}
