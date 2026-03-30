#[test]
#[cfg(feature = "server")]
fn help() {
    use fabro_test::{fabro_snapshot, test_context};

    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["serve", "--help"]);
    fabro_snapshot!(context.filters(), cmd, @"");
}
