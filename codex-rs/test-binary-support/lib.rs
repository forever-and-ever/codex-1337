use std::path::Path;
use std::sync::Mutex;
use std::sync::PoisonError;

use codex_arg0::Arg0DispatchPaths;
use codex_arg0::Arg0PathEntryGuard;
use codex_arg0::arg0_dispatch;
use tempfile::TempDir;

static TEST_CODEX_HOMES: Mutex<Vec<TempDir>> = Mutex::new(Vec::new());

unsafe extern "C" {
    fn atexit(callback: extern "C" fn()) -> std::ffi::c_int;
}

extern "C" fn cleanup_test_codex_homes() {
    TEST_CODEX_HOMES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
}

pub struct TestBinaryDispatchGuard {
    arg0: Arg0PathEntryGuard,
}

impl TestBinaryDispatchGuard {
    pub fn paths(&self) -> &Arg0DispatchPaths {
        self.arg0.paths()
    }
}

pub enum TestBinaryDispatchMode {
    DispatchArg0Only,
    Skip,
    InstallAliases,
}

pub fn configure_test_binary_dispatch<F>(
    codex_home_prefix: &str,
    classify: F,
) -> Option<TestBinaryDispatchGuard>
where
    F: FnOnce(&str, Option<&str>) -> TestBinaryDispatchMode,
{
    let mut args = std::env::args_os();
    let argv0 = args.next().unwrap_or_default();
    let exe_name = Path::new(&argv0)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let argv1 = args.next();
    match classify(exe_name, argv1.as_deref().and_then(|arg| arg.to_str())) {
        TestBinaryDispatchMode::DispatchArg0Only => {
            let _ = arg0_dispatch();
            None
        }
        TestBinaryDispatchMode::Skip => None,
        TestBinaryDispatchMode::InstallAliases => {
            let codex_home = match tempfile::Builder::new().prefix(codex_home_prefix).tempdir() {
                Ok(codex_home) => codex_home,
                Err(error) => panic!("failed to create test CODEX_HOME: {error}"),
            };
            let previous_codex_home = std::env::var_os("CODEX_HOME");
            // Safety: this runs from a test ctor before test threads begin.
            unsafe {
                std::env::set_var("CODEX_HOME", codex_home.path());
            }

            let arg0 = match arg0_dispatch() {
                Some(arg0) => arg0,
                None => panic!("failed to configure arg0 dispatch aliases for test binary"),
            };
            match previous_codex_home.as_ref() {
                Some(value) => unsafe {
                    std::env::set_var("CODEX_HOME", value);
                },
                None => unsafe {
                    std::env::remove_var("CODEX_HOME");
                },
            }

            // Aliases live under CODEX_HOME/tmp/arg0; the ctor's static guard is
            // never dropped. Keep the home until tests and their children finish.
            // SAFETY: this runs in the single-threaded ctor, and the callback has
            // the C ABI and remains valid until process exit.
            assert_eq!(
                unsafe { atexit(cleanup_test_codex_homes) },
                0,
                "failed to register test CODEX_HOME cleanup"
            );
            TEST_CODEX_HOMES
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(codex_home);

            Some(TestBinaryDispatchGuard { arg0 })
        }
    }
}
