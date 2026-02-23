use assert_cmd::Command;
use predicates::prelude::*;

#[allow(deprecated)]
fn attractor() -> Command {
    Command::cargo_bin("attractor").unwrap()
}

// -- validate ----------------------------------------------------------------

#[test]
fn validate_simple() {
    attractor()
        .args(["validate", "../../test/simple.dot"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Validation: OK"));
}

#[test]
fn validate_branching() {
    attractor()
        .args(["validate", "../../test/branching.dot"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Validation: OK"));
}

#[test]
fn validate_conditions() {
    attractor()
        .args(["validate", "../../test/conditions.dot"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Validation: OK"));
}

#[test]
fn validate_parallel() {
    attractor()
        .args(["validate", "../../test/parallel.dot"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Validation: OK"));
}

#[test]
fn validate_styled() {
    attractor()
        .args(["validate", "../../test/styled.dot"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Validation: OK"));
}

#[test]
fn validate_invalid() {
    attractor()
        .args(["validate", "../../test/invalid.dot"])
        .assert()
        .failure();
}

// -- run --dry-run -----------------------------------------------------------

#[test]
fn dry_run_simple() {
    attractor()
        .args(["run", "--dry-run", "--auto-approve", "../../test/simple.dot"])
        .assert()
        .success();
}

#[test]
fn dry_run_branching() {
    attractor()
        .args(["run", "--dry-run", "--auto-approve", "../../test/branching.dot"])
        .assert()
        .success();
}

#[test]
fn dry_run_conditions() {
    attractor()
        .args(["run", "--dry-run", "--auto-approve", "../../test/conditions.dot"])
        .assert()
        .success();
}

#[test]
fn dry_run_parallel() {
    attractor()
        .args(["run", "--dry-run", "--auto-approve", "../../test/parallel.dot"])
        .assert()
        .success();
}

#[test]
fn dry_run_styled() {
    attractor()
        .args(["run", "--dry-run", "--auto-approve", "../../test/styled.dot"])
        .assert()
        .success();
}
