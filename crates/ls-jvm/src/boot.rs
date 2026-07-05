//! The embedded-JVM boot: assemble the plan's VM options, `dlopen` libjvm, and
//! call the single boot symbol `JNI_CreateJavaVM`. The premain fires inside that
//! call, reads the Rust vtable address from the agent argument, and registers
//! the PC vtable over FFM; the caller then rendezvouses on that registration.

use std::ffi::{c_void, CString};
use std::path::{Path, PathBuf};

use crate::jni::{JavaVmInitArgs, JavaVmOption, JniCreateJavaVm, JNI_VERSION_21};

/// A boundary boot failure.
#[derive(Debug)]
pub enum BootError {
    /// `dlopen`/`dlsym`/`JNI_CreateJavaVM` failed.
    Boot(String),
    /// The premain never completed registration before the deadline; the
    /// captured island log is surfaced by the doctor.
    RendezvousTimeout { island_log: Vec<String> },
    /// The island refused registration on an ABI-version mismatch.
    AbiMismatch,
}

impl std::fmt::Display for BootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootError::Boot(m) => write!(f, "boot failed: {m}"),
            BootError::RendezvousTimeout { island_log } => write!(
                f,
                "island registration timed out; island log: [{}]",
                island_log.join(" | ")
            ),
            BootError::AbiMismatch => {
                write!(f, "island refused registration: ABI version mismatch")
            }
        }
    }
}

impl std::error::Error for BootError {}

/// The exact VM options the plan mandates, in order: the agent (and any extra
/// PC-host assembly) on the class path, native-access enabled for FFM, compact
/// object headers, the workspace root (when known) so the island can find its
/// per-workspace plugin config, any caller-supplied extra `-D`/JVM options, and
/// the `-javaagent` carrying the Rust vtable address as its argument. Pure
/// string assembly, so it is unit-tested without booting a JVM.
pub fn boot_options(
    agent_jar: &Path,
    extra_classpath: &[PathBuf],
    vtable_addr: usize,
    workspace_root: Option<&Path>,
    extra_jvm_options: &[String],
) -> Vec<String> {
    let mut class_path = agent_jar.display().to_string();
    for entry in extra_classpath {
        class_path.push(':');
        class_path.push_str(&entry.display().to_string());
    }
    let mut options = vec![
        format!("-Djava.class.path={class_path}"),
        "--enable-native-access=ALL-UNNAMED".to_string(),
        "-XX:+UseCompactObjectHeaders".to_string(),
    ];
    // Hand the island its workspace root so the premain loads
    // `<root>/.scala3-bsp-semantic-ls/pc-plugins.json`; without it the island
    // runs with ephemeral (config-less) settings.
    if let Some(root) = workspace_root {
        options.push(format!("-Dls.pc.host.workspace={}", root.display()));
    }
    // Caller-supplied JVM options (e.g. tuning flags, or a test fault property);
    // kept before the agent so the `-javaagent` stays last.
    options.extend(extra_jvm_options.iter().cloned());
    options.push(format!(
        "-javaagent:{}=0x{vtable_addr:x}",
        agent_jar.display()
    ));
    options
}

/// Does this process currently have `libjvm` mapped? Read from
/// `/proc/self/maps`, so it reflects the real cold-start property (the JVM is
/// absent until the first PC request boots it).
pub fn libjvm_mapped() -> bool {
    std::fs::read_to_string("/proc/self/maps")
        .map(|maps| maps.contains("libjvm"))
        .unwrap_or(false)
}

/// `dlopen` libjvm and call `JNI_CreateJavaVM` with `options`. The premain runs
/// synchronously inside this call; the returned `JavaVM*`/`JNIEnv*` are ignored
/// (teardown is process exit). On success libjvm stays mapped for the process
/// lifetime.
pub fn create_java_vm(libjvm: &Path, options: &[String]) -> Result<(), String> {
    let c_options: Vec<CString> = options
        .iter()
        .map(|opt| CString::new(opt.as_str()).map_err(|e| format!("option {opt:?}: {e}")))
        .collect::<Result<_, _>>()?;

    // SAFETY: `dlopen`/`dlsym` of a real libjvm; the CString options and the
    // options array outlive the `JNI_CreateJavaVM` call; the arg pointers are
    // valid `#[repr(C)]` structs matching the JNI ABI.
    unsafe {
        let lib =
            libloading::Library::new(libjvm).map_err(|e| format!("dlopen {libjvm:?}: {e}"))?;
        let create: libloading::Symbol<JniCreateJavaVm> = lib
            .get(b"JNI_CreateJavaVM\0")
            .map_err(|e| format!("dlsym JNI_CreateJavaVM: {e}"))?;

        let mut vm_options: Vec<JavaVmOption> = c_options
            .iter()
            .map(|opt| JavaVmOption {
                option_string: opt.as_ptr(),
                extra_info: std::ptr::null_mut(),
            })
            .collect();

        let mut args = JavaVmInitArgs {
            version: JNI_VERSION_21,
            n_options: vm_options.len() as i32,
            options: vm_options.as_mut_ptr(),
            ignore_unrecognized: 0,
        };

        let mut jvm: *mut c_void = std::ptr::null_mut();
        let mut env: *mut c_void = std::ptr::null_mut();
        let rc = create(
            &mut jvm,
            &mut env,
            (&mut args as *mut JavaVmInitArgs).cast::<c_void>(),
        );
        if rc != 0 {
            return Err(format!("JNI_CreateJavaVM failed rc={rc}"));
        }

        // The JVM lives for the process lifetime; keep libjvm loaded.
        std::mem::forget(lib);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_options_are_exactly_the_plan_option_set() {
        let opts = boot_options(Path::new("/opt/pc-host.jar"), &[], 0xdead_beef, None, &[]);
        assert_eq!(
            opts,
            vec![
                "-Djava.class.path=/opt/pc-host.jar".to_string(),
                "--enable-native-access=ALL-UNNAMED".to_string(),
                "-XX:+UseCompactObjectHeaders".to_string(),
                "-javaagent:/opt/pc-host.jar=0xdeadbeef".to_string(),
            ]
        );
    }

    #[test]
    fn extra_classpath_is_appended_after_the_agent_jar() {
        let opts = boot_options(
            Path::new("/a/agent.jar"),
            &[PathBuf::from("/b/pc.jar"), PathBuf::from("/c/x.jar")],
            0x10,
            None,
            &[],
        );
        assert_eq!(opts[0], "-Djava.class.path=/a/agent.jar:/b/pc.jar:/c/x.jar");
        // The agent argument still points at the agent jar, not the extras.
        assert_eq!(opts[3], "-javaagent:/a/agent.jar=0x10");
    }

    #[test]
    fn workspace_root_is_passed_as_a_system_property_before_the_agent() {
        let opts = boot_options(
            Path::new("/opt/pc-host.jar"),
            &[],
            0x20,
            Some(Path::new("/home/u/project")),
            &[],
        );
        assert_eq!(
            opts,
            vec![
                "-Djava.class.path=/opt/pc-host.jar".to_string(),
                "--enable-native-access=ALL-UNNAMED".to_string(),
                "-XX:+UseCompactObjectHeaders".to_string(),
                "-Dls.pc.host.workspace=/home/u/project".to_string(),
                "-javaagent:/opt/pc-host.jar=0x20".to_string(),
            ]
        );
    }

    #[test]
    fn extra_jvm_options_are_inserted_before_the_agent() {
        let opts = boot_options(
            Path::new("/opt/pc-host.jar"),
            &[],
            0x30,
            Some(Path::new("/ws")),
            &["-Dls.pc.host.testFault=busyCompletion".to_string()],
        );
        assert_eq!(
            opts,
            vec![
                "-Djava.class.path=/opt/pc-host.jar".to_string(),
                "--enable-native-access=ALL-UNNAMED".to_string(),
                "-XX:+UseCompactObjectHeaders".to_string(),
                "-Dls.pc.host.workspace=/ws".to_string(),
                "-Dls.pc.host.testFault=busyCompletion".to_string(),
                "-javaagent:/opt/pc-host.jar=0x30".to_string(),
            ]
        );
    }

    #[test]
    fn libjvm_is_not_mapped_in_a_cold_test_process() {
        // These unit tests never boot a JVM, so libjvm must be absent — the
        // cold-start (zero-JVM) property.
        assert!(!libjvm_mapped());
    }
}
