//! Test plugin compiled with `rustc --crate-type cdylib` by the Native loader
//! tests. Keep the declarations independent of the Rust contract crate:
//! the ABI must also work for third-party languages.
use std::ffi::c_void;
use std::time::Duration;

#[repr(C)]
struct Header {
    revision: u32,
    size: u32,
}
#[repr(C)]
struct Target {
    pointer_width: u32,
    byte_order: u32,
}
#[repr(C)]
struct HostAllocator {
    context: *mut c_void,
    allocate: unsafe extern "C" fn(*mut c_void, usize) -> *mut u8,
}
#[repr(C)]
struct CallOutput {
    data: *mut u8,
    length: usize,
}
#[repr(C)]
struct PluginAbi {
    header: Header,
    target: Target,
    create: unsafe extern "C" fn() -> *mut c_void,
    destroy: unsafe extern "C" fn(*mut c_void),
    supports: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> bool,
    invoke: unsafe extern "C" fn(
        *mut c_void,
        *const u8,
        usize,
        *const u8,
        usize,
        *const c_void,
        HostAllocator,
        *mut CallOutput,
    ) -> i32,
}

unsafe extern "C" fn create() -> *mut c_void {
    Box::into_raw(Box::new(0u8)).cast()
}
unsafe extern "C" fn destroy(instance: *mut c_void) {
    if !instance.is_null() {
        drop(unsafe { Box::from_raw(instance.cast::<u8>()) });
    }
}
unsafe extern "C" fn supports(_: *mut c_void, name: *const u8, len: usize) -> bool {
    if name.is_null() || len > 100 {
        return false;
    }
    matches!(
        unsafe { std::slice::from_raw_parts(name, len) },
        b"initialize" | b"shutdown" | b"probe" | b"extract_metadata"
    )
}
unsafe extern "C" fn invoke(
    _: *mut c_void,
    name: *const u8,
    len: usize,
    input: *const u8,
    input_len: usize,
    _host_api: *const c_void,
    host: HostAllocator,
    output: *mut CallOutput,
) -> i32 {
    if name.is_null() || output.is_null() || len > 100 || input.is_null() || input_len > 1024 * 1024
    {
        return -1;
    }
    let name = unsafe { std::slice::from_raw_parts(name, len) };
    let input = unsafe { std::slice::from_raw_parts(input, input_len) };
    if name == b"probe" && input.windows(6).any(|window| window == b"\"slow\"") {
        std::thread::sleep(Duration::from_millis(180));
    }
    let value: &[u8] =
        if name == b"probe" && input.windows(10).any(|window| window == b"\"oversize\"") {
            return if unsafe { (host.allocate)(host.context, 2 * 1024 * 1024) }.is_null() {
                -2
            } else {
                -3
            };
        } else {
            b"{\"kind\":\"no_match\"}"
        };
    let data = unsafe { (host.allocate)(host.context, value.len()) };
    if data.is_null() {
        return -4;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(value.as_ptr(), data, value.len());
        *output = CallOutput {
            data,
            length: value.len(),
        };
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ting_plugin_abi_v2() -> *const c_void {
    static ABI: PluginAbi = PluginAbi {
        header: Header {
            revision: if cfg!(bad_revision) { 1 } else { 2 },
            size: std::mem::size_of::<PluginAbi>() as u32,
        },
        target: Target {
            pointer_width: usize::BITS,
            byte_order: if cfg!(target_endian = "little") { 1 } else { 2 },
        },
        create,
        destroy,
        supports,
        invoke,
    };
    (&ABI as *const PluginAbi).cast()
}
