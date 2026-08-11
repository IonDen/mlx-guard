#![allow(unsafe_code)]

use std::io::Read;
use std::os::fd::FromRawFd;

use mlx_guard_core::{ClientReady, ControlErrorKind};

#[test]
fn readiness_descriptor_is_close_on_exec_and_notifies_with_eof() {
    // Catches leaking the client descriptor into the worker or writing a SIGPIPE-prone byte.
    let mut descriptors = [-1; 2];
    // SAFETY: the array has space for the two descriptors written by pipe.
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    // SAFETY: successful pipe returned two distinct owned descriptors.
    let mut read = unsafe { std::fs::File::from_raw_fd(descriptors[0]) };
    let write = descriptors[1];

    let ready = ClientReady::take(Some(write)).unwrap();
    // SAFETY: write remains live until ready is notified.
    let flags = unsafe { libc::fcntl(write, libc::F_GETFD) };
    assert_ne!(flags, -1);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);

    ready.notify();
    let mut byte = [0_u8; 1];
    assert_eq!(read.read(&mut byte).unwrap(), 0);
}

#[test]
fn invalid_readiness_descriptor_is_rejected_without_ownership() {
    // Catches wrapping an invalid raw descriptor in an owned Rust value.
    let error = ClientReady::take(Some(-1)).unwrap_err();
    assert_eq!(error.kind(), ControlErrorKind::ClientReadyUnavailable);
}

#[test]
fn standard_io_descriptor_is_rejected_by_the_core_boundary() {
    // Catches a direct core caller bypassing the CLI's reserved-descriptor validation.
    // SAFETY: fork creates an isolated child so a broken implementation cannot close parent stderr.
    let child = unsafe { libc::fork() };
    assert_ne!(child, -1);
    if child == 0 {
        let accepted = ClientReady::take(Some(libc::STDERR_FILENO)).is_ok();
        // SAFETY: the child has no Rust state to unwind and reports only this validation result.
        unsafe { libc::_exit(i32::from(accepted)) }
    }
    let mut status = 0;
    // SAFETY: `child` is the live child PID returned by fork and status is writable.
    assert_eq!(unsafe { libc::waitpid(child, &raw mut status, 0) }, child);
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 0);
}
