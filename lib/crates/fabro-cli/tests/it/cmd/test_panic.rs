use fabro_test::test_context;

#[test]
fn builds_event_and_roundtrips_through_json() {
    let context = test_context!();
    let mut cmd = context.command();
    cmd.args(["__test_panic", "test panic message"]);

    let output = cmd.output().expect("command should execute");
    assert!(output.status.success(), "command failed: {output:?}");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let event: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(event["level"], "fatal");

    let exception = &event["exception"]["values"][0];
    assert_eq!(exception["type"], "panic");
    assert_eq!(exception["value"], "test panic message");

    let mechanism = &exception["mechanism"];
    assert_eq!(mechanism["type"], "panic");
    assert_eq!(mechanism["handled"], false);

    assert!(
        exception["stacktrace"].is_object(),
        "stacktrace should be present"
    );
    assert!(
        event["contexts"]["os"].is_object(),
        "OS context should be present"
    );
}
