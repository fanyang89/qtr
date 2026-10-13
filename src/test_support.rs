//! Process isolation for tests that mutate process-global native state.

use std::{env, process::Command, thread};

const ISOLATED_TEST: &str = "QTR_ISOLATED_TEST";

/// Returns true in the parent after running this exact test in a fresh process.
/// Call at the start of a test and return immediately when true. No environment
/// variables are changed in the parallel parent test process.
pub(crate) fn in_subprocess() -> bool {
    let current = thread::current();
    let name = current.name().expect("libtest must name the test thread");
    let marker = format!("qtr isolated test entered: {name}");
    if let Ok(child_test) = env::var(ISOLATED_TEST) {
        assert_eq!(child_test, name, "isolated child ran the wrong test");
        println!("\n{marker}");
        return false;
    }

    let output = Command::new(env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(ISOLATED_TEST, name)
        .output()
        .expect("failed to start isolated test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.lines().any(|line| line == marker),
        "isolated test {name} failed or did not run: {}\n{stdout}\n{stderr}",
        output.status
    );
    true
}

#[test]
fn isolation_runs_the_named_test_in_a_child() {
    if in_subprocess() {
        return;
    }
    assert_eq!(
        env::var(ISOLATED_TEST).unwrap(),
        "test_support::isolation_runs_the_named_test_in_a_child"
    );
}
