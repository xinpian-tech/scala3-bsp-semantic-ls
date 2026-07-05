//! The only JNI surface: the Invocation-API boot symbol and its argument
//! structs, hand-declared `#[repr(C)]` (no `jni.h`, no bindgen, no `jni` crate).
//! The `JavaVM*`/`JNIEnv*` that `JNI_CreateJavaVM` returns are ignored — the
//! entire boundary is driven over FFM once the premain registers the PC vtable.

use std::ffi::{c_char, c_void};

/// `JavaVMOption` — one VM option string.
#[repr(C)]
pub struct JavaVmOption {
    pub option_string: *const c_char,
    pub extra_info: *mut c_void,
}

/// `JavaVMInitArgs` — the argument block for `JNI_CreateJavaVM`.
#[repr(C)]
pub struct JavaVmInitArgs {
    pub version: i32,
    pub n_options: i32,
    pub options: *mut JavaVmOption,
    pub ignore_unrecognized: u8,
}

/// The one boot symbol we resolve: `JNI_CreateJavaVM(JavaVM**, void**, void*)`.
pub type JniCreateJavaVm =
    unsafe extern "C" fn(p_vm: *mut *mut c_void, p_env: *mut *mut c_void, args: *mut c_void) -> i32;

/// JNI version requested in `JavaVMInitArgs` (JDK 21+ invocation semantics).
pub const JNI_VERSION_21: i32 = 0x0015_0000;
