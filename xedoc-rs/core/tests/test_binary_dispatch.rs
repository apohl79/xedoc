use ctor::ctor;
use xedoc_apply_patch::XEDOC_CORE_APPLY_PATCH_ARG1;
use xedoc_exec_server::XEDOC_FS_HELPER_ARG1;
use xedoc_sandboxing::landlock::XEDOC_LINUX_SANDBOX_ARG0;
use xedoc_test_binary_support::TestBinaryDispatchGuard;
use xedoc_test_binary_support::TestBinaryDispatchMode;
use xedoc_test_binary_support::configure_test_binary_dispatch;

/// Lets every integration-test binary dispatch the helper aliases it may invoke.
#[ctor]
pub static XEDOC_ALIASES_TEMP_DIR: Option<TestBinaryDispatchGuard> = {
    configure_test_binary_dispatch("xedoc-core-tests", |exe_name, argv1| {
        if argv1 == Some(XEDOC_CORE_APPLY_PATCH_ARG1) {
            return TestBinaryDispatchMode::DispatchArg0Only;
        }
        if argv1 == Some(XEDOC_FS_HELPER_ARG1) {
            return TestBinaryDispatchMode::DispatchArg0Only;
        }
        if exe_name == XEDOC_LINUX_SANDBOX_ARG0 {
            return TestBinaryDispatchMode::DispatchArg0Only;
        }
        TestBinaryDispatchMode::InstallAliases
    })
};
