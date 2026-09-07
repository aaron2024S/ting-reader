/// Release memory to the OS (Linux only)
///
/// This function calls `malloc_trim(0)` to release free memory back to the OS.
/// This is useful for long-running processes that allocate and free large amounts of memory,
/// as the glibc allocator may hold onto memory for reuse.
pub fn release_memory() {
    #[cfg(target_os = "linux")]
    {
        unsafe {
            libc::malloc_trim(0);
        }
    }
}
