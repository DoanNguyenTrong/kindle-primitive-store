use std::{
    ffi::CString,
    os::unix::ffi::OsStrExt,
    path::Path,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

static LOG_PATH: AtomicPtr<libc::c_char> = AtomicPtr::new(ptr::null_mut());

/// Keep the path for the lifetime of the process. The log is opened only on a crash,
/// so the running Store has no file descriptor open on user storage while idle.
pub fn install(path: &Path) {
    if let Ok(path) = CString::new(path.as_os_str().as_bytes()) {
        LOG_PATH.store(path.into_raw(), Ordering::Relaxed);
    }
    // SAFETY: sigaction is initialized below, with a handler matching SA_SIGINFO.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_fatal as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGBUS, &action, ptr::null_mut());
        libc::sigaction(libc::SIGSEGV, &action, ptr::null_mut());
    }
}

extern "C" fn on_fatal(signal: libc::c_int, _info: *mut libc::siginfo_t, _context: *mut libc::c_void) {
    let message = if signal == libc::SIGBUS {
        b"[store crash] SIGBUS\n".as_slice()
    } else {
        b"[store crash] SIGSEGV\n".as_slice()
    };
    let path = LOG_PATH.load(Ordering::Relaxed);
    // SAFETY: The path is a live C string or null. These libc calls are async-signal-safe.
    unsafe {
        libc::write(libc::STDERR_FILENO, message.as_ptr().cast(), message.len());
        if !path.is_null() {
            let fd = libc::open(path, libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT, 0o644);
            if fd >= 0 {
                libc::write(fd, message.as_ptr().cast(), message.len());
                libc::close(fd);
            }
        }
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}
