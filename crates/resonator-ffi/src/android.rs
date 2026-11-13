//! Android: hands the JavaVM and the app's Context to `ndk-context`, which
//! the DNS resolver (hickory) and network monitoring (netdev) use to read
//! the system's DNS servers and interfaces. Without it the node fails to
//! start with "android context was not initialized".
//!
//! The app calls `System.loadLibrary("resonator_ffi")` before the first
//! call through the bindings; Android then runs [`JNI_OnLoad`] with the VM,
//! and the Context is the running Application.

use jni::JavaVM;
use jni::objects::JObject;
use jni::sys::{JNI_VERSION_1_6, jint};
use std::ffi::c_void;

/// Called by Android when the library is loaded through `System.loadLibrary`.
///
/// # Safety
/// Called by the Java runtime with a valid `JavaVM` pointer.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn JNI_OnLoad(vm: JavaVM, _reserved: *mut c_void) -> jint {
    match application(&vm) {
        Ok(context) => {
            // SAFETY: the VM pointer is valid for the life of the process and
            // `context` is a global reference that is never released.
            unsafe {
                ndk_context::initialize_android_context(
                    vm.get_java_vm_pointer().cast(),
                    context.as_obj().as_raw().cast(),
                );
            }
            std::mem::forget(context);
        }
        Err(error) => tracing::warn!(%error, "no Android context for the node"),
    }
    JNI_VERSION_1_6
}

/// The running Application (`ActivityThread.currentApplication()`), as a
/// global reference.
fn application(vm: &JavaVM) -> jni::errors::Result<jni::objects::GlobalRef> {
    let mut env = vm.get_env()?;
    let app: JObject = env
        .call_static_method(
            "android/app/ActivityThread",
            "currentApplication",
            "()Landroid/app/Application;",
            &[],
        )?
        .l()?;
    if app.is_null() {
        return Err(jni::errors::Error::NullPtr("currentApplication"));
    }
    env.new_global_ref(app)
}
