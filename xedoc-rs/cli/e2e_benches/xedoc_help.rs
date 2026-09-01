#![allow(clippy::expect_used)]

use std::process::Command;

use divan::Bencher;

fn main() {
    divan::main();
}

/// Exercises the Bazel-backed end-to-end benchmark path with a cheap,
/// deterministic Xedoc invocation. Richer scenarios can add separate
/// benchmark binaries without making the shared harness depend on them.
#[divan::bench(sample_count = 20, sample_size = 1)]
fn xedoc_help(bencher: Bencher) {
    let xedoc = xedoc_utils_cargo_bin::cargo_bin("xedoc")
        .expect("xedoc binary should be available through Bazel runfiles");

    bencher.bench_local(move || {
        let output = Command::new(&xedoc)
            .arg("--help")
            .output()
            .expect("xedoc --help should run");
        assert!(output.status.success(), "xedoc --help should succeed");
    });
}
