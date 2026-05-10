//! Thread spawn/join runtime support (ADR-0084).
//!
//! Backed by pthreads on Unix. The compiler's `@spawn(fn, arg)` codegen
//! emits:
//!
//! 1. A per-(arg type, return type) thunk with the C
//!    `void *(*)(void *)` signature. The thunk reads the arg out of
//!    the slot pointer it receives, calls the spawned Gruel function,
//!    boxes the return value on the heap, and returns that box. It
//!    also frees the arg slot before returning.
//! 2. A call to `__gruel_thread_spawn(thunk, arg_buf)` returning an
//!    opaque `*mut u8` handle.
//!
//! Joining via `JoinHandle::join(self) -> R` lowers to
//! `__gruel_thread_join(handle, ret_buf)` where `ret_buf` is a
//! caller-owned `R`-sized stack slot. The runtime joins the pthread,
//! reads the return-value pointer from `pthread_join`'s out-param,
//! memcpys `ret_size` bytes into `ret_buf`, and frees the heap-boxed
//! return value plus the handle.

use crate::platform;

// =============================================================================
// pthread FFI (Unix only)
// =============================================================================

// pthread_t differs across platforms — `usize` matches the size on
// x86_64-linux (`unsigned long`) and aarch64-darwin (pointer to
// opaque struct). The value is opaque to us; we only round-trip it
// through pthread_create / pthread_join.
type PthreadT = usize;

unsafe extern "C" {
    fn pthread_create(
        thread: *mut PthreadT,
        attr: *const u8,
        start_routine: extern "C" fn(*mut u8) -> *mut u8,
        arg: *mut u8,
    ) -> i32;

    fn pthread_join(thread: PthreadT, retval: *mut *mut u8) -> i32;
}

// =============================================================================
// ThreadHandle: per-spawn bookkeeping owned by the JoinHandle
// =============================================================================

#[repr(C)]
struct ThreadHandle {
    pthread: PthreadT,
    ret_size: usize,
}

// =============================================================================
// FFI: __gruel_thread_spawn
// =============================================================================

/// Spawn a new thread running `thunk`.
///
/// Parameters:
///
/// - `thunk`: codegen-emitted trampoline with the C `void*(*)(void*)`
///   signature. Receives `arg_buf` as its parameter, reads the arg out
///   of it, calls the spawned function, boxes the return value with
///   `__gruel_alloc`, and returns the box pointer (or null if the
///   return type is unit). The thunk is responsible for freeing
///   `arg_buf` before returning.
/// - `arg_buf`: caller-allocated buffer containing the argument. May
///   be null when the argument is zero-sized; the thunk treats it as
///   such.
/// - `ret_size`: byte size of the return value. Stored in the
///   ThreadHandle so the join site knows how much to memcpy.
///
/// Returns: opaque `*mut u8` handle. Aborts the process on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __gruel_thread_spawn(
    thunk: extern "C" fn(*mut u8) -> *mut u8,
    arg_buf: *mut u8,
    ret_size: usize,
) -> *mut u8 {
    let handle_buf = unsafe { platform::malloc(core::mem::size_of::<ThreadHandle>()) };
    if handle_buf.is_null() {
        platform::write_stderr(b"thread spawn: failed to allocate handle\n");
        platform::exit(101);
    }
    let handle = handle_buf as *mut ThreadHandle;

    let mut tid: PthreadT = 0;
    let rc =
        unsafe { pthread_create(&mut tid as *mut PthreadT, core::ptr::null(), thunk, arg_buf) };
    if rc != 0 {
        platform::write_stderr(b"thread spawn: pthread_create failed\n");
        platform::exit(101);
    }

    unsafe {
        (*handle).pthread = tid;
        (*handle).ret_size = ret_size;
    }
    handle as *mut u8
}

// =============================================================================
// FFI: __gruel_thread_join
// =============================================================================

/// Join a thread and copy its return value into `ret_out`.
///
/// Parameters:
///
/// - `handle_ptr`: handle from `__gruel_thread_spawn`. Consumed
///   (freed before return).
/// - `ret_out`: caller-owned buffer to receive the return value. May
///   be null when the return type is unit (the thunk returns null).
///
/// Aborts on `pthread_join` failure — the JoinHandle posture has no
/// error channel today.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __gruel_thread_join(handle_ptr: *mut u8, ret_out: *mut u8) {
    let handle = handle_ptr as *mut ThreadHandle;
    let mut retval: *mut u8 = core::ptr::null_mut();
    let rc = unsafe { pthread_join((*handle).pthread, &mut retval as *mut *mut u8) };
    if rc != 0 {
        platform::write_stderr(b"thread join: pthread_join failed\n");
        platform::exit(101);
    }
    unsafe {
        let ret_size = (*handle).ret_size;
        if ret_size > 0 && !ret_out.is_null() && !retval.is_null() {
            core::ptr::copy_nonoverlapping(retval, ret_out, ret_size);
        }
        if !retval.is_null() {
            platform::free(retval);
        }
        platform::free(handle_ptr);
    }
}
