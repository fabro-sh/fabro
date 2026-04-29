use fabro_config::LogFilter;

#[test]
fn log_filter_accepts_env_filter_directives() {
    let filter = LogFilter::parse("info,fabro_server=debug")
        .expect("valid env filter directive should parse");

    assert_eq!(filter.as_str(), "info,fabro_server=debug");
}

#[test]
fn log_filter_rejects_invalid_directives() {
    LogFilter::parse("definitely not a filter")
        .expect_err("whitespace phrase should not be accepted as a filter");
    LogFilter::parse("fabro_server=definitelynotalevel")
        .expect_err("unknown level should not be accepted as a filter");
}

#[test]
fn log_filter_round_trips_through_serde() {
    let filter = LogFilter::parse("warn,fabro_cli=debug").unwrap();

    let json = serde_json::to_value(&filter).expect("filter should serialize");
    assert_eq!(json, "warn,fabro_cli=debug");

    let round_trip: LogFilter = serde_json::from_value(json).expect("filter should deserialize");
    assert_eq!(round_trip, filter);
}
