load("@prelude//tests:remote_test_execution_toolchain.bzl", "RemoteTestExecutionToolchainInfo")

def _noop_remote_test_execution_toolchain_impl(ctx):
    return [
        DefaultInfo(),
        RemoteTestExecutionToolchainInfo(
            profiles = {},
        ),
    ]

noop_remote_test_execution_toolchain = rule(
    impl = _noop_remote_test_execution_toolchain_impl,
    attrs = {},
    is_toolchain_rule = True,
)
