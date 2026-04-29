use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::Args;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use super::{PlannedCommand, capture_command, command, workspace_root};

const TOOL_CONFIG_RELATIVE: &str = "target/bench-tests/nextest-tool.toml";
const TOOL_CONFIG_BODY: &str = "\
[profile.bench]
# Synthesized by `cargo dev bench-tests`. Lenient timeouts so slow tests
# aren't killed mid-run, polluting timing data.
slow-timeout = { period = \"30s\", terminate-after = 4 }
leak-timeout = \"2s\"
junit = { path = \"junit.xml\" }
";

#[derive(Debug, Args)]
pub(crate) struct BenchTestsArgs {
    /// Number of full test-suite runs to execute.
    #[arg(long, default_value_t = 5)]
    runs: u32,

    /// Nextest profile that writes JUnit XML. The default `bench` profile is
    /// synthesized at runtime via `--tool-config-file` and does not need to
    /// be present in `.config/nextest.toml`.
    #[arg(long, default_value = "bench")]
    profile: String,

    /// Output CSV path. Header is written when the file is created; rows are
    /// appended on subsequent invocations.
    #[arg(long, value_name = "PATH")]
    output: PathBuf,

    /// Optional nextest filterset (e.g. "package(fabro-cli)").
    #[arg(long, value_name = "FILTERSET")]
    filter: Option<String>,

    /// Extra arguments forwarded to `cargo nextest run`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    nextest_args: Vec<String>,
}

#[expect(
    clippy::print_stderr,
    reason = "bench-tests reports per-run progress to stderr"
)]
#[expect(
    clippy::disallowed_methods,
    reason = "fabro-dev is a synchronous CLI; reading the JUnit report and removing stale files is intentional sync I/O"
)]
pub(crate) fn bench_tests(args: BenchTestsArgs) -> Result<()> {
    let BenchTestsArgs {
        runs,
        profile,
        output,
        filter,
        nextest_args,
    } = args;
    if runs == 0 {
        bail!("--runs must be at least 1");
    }

    let root = workspace_root();
    let git_sha = resolve_head_sha(&root)?;
    let tool_config = ensure_tool_config(&root)?;
    let junit_path = root
        .join("target")
        .join("nextest")
        .join(&profile)
        .join("junit.xml");

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    let mut writer = open_csv_writer(&output)?;

    for run_index in 1..=runs {
        eprintln!("[bench-tests] run {run_index}/{runs} sha={git_sha} profile={profile}");

        if junit_path.exists() {
            fs::remove_file(&junit_path)
                .with_context(|| format!("removing stale {}", junit_path.display()))?;
        }

        let started_at = Utc::now().to_rfc3339();
        let nextest =
            build_nextest_command(&profile, filter.as_deref(), &nextest_args, &tool_config);
        let mut child = command(&nextest)
            .current_dir(&root)
            .stdin(Stdio::null())
            .spawn()
            .with_context(|| format!("spawning {}", nextest.to_shell_line()))?;
        let status = child
            .wait()
            .with_context(|| format!("waiting on {}", nextest.to_shell_line()))?;
        if !junit_path.exists() {
            bail!(
                "nextest exited with {status} and did not write {} — likely a build failure",
                junit_path.display()
            );
        }
        if !status.success() {
            eprintln!("[bench-tests] nextest exited {status}; recording results from JUnit anyway");
        }

        let xml = fs::read_to_string(&junit_path)
            .with_context(|| format!("reading {}", junit_path.display()))?;
        let cases =
            parse_junit(&xml).with_context(|| format!("parsing {}", junit_path.display()))?;

        for case in &cases {
            writer
                .write_record([
                    git_sha.as_str(),
                    &run_index.to_string(),
                    started_at.as_str(),
                    case.binary.as_str(),
                    case.package.as_str(),
                    case.classname.as_str(),
                    case.test_name.as_str(),
                    case.status.as_str(),
                    &case.duration_ms.to_string(),
                ])
                .context("writing CSV row")?;
        }
        writer.flush().context("flushing CSV writer")?;

        let totals = Totals::tally(&cases);
        eprintln!(
            "[bench-tests] run {run_index} wrote {count} rows ({totals})",
            count = cases.len(),
        );
    }

    Ok(())
}

fn build_nextest_command(
    profile: &str,
    filter: Option<&str>,
    extra_args: &[String],
    tool_config: &Path,
) -> PlannedCommand {
    let mut cmd = PlannedCommand::new("cargo")
        .arg("nextest")
        .arg("run")
        .arg("--workspace")
        .arg("--no-fail-fast")
        .arg("--profile")
        .arg(profile)
        .arg("--tool-config-file")
        .arg(format!("bench-tests:{}", tool_config.display()));
    if let Some(filter) = filter {
        cmd = cmd.arg("--filterset").arg(filter);
    }
    for extra in extra_args {
        cmd = cmd.arg(extra);
    }
    cmd
}

#[expect(
    clippy::disallowed_methods,
    reason = "fabro-dev synchronously writes a tool-config TOML to target/ before each bench run"
)]
fn ensure_tool_config(root: &Path) -> Result<PathBuf> {
    let path = root.join(TOOL_CONFIG_RELATIVE);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(&path, TOOL_CONFIG_BODY).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

fn resolve_head_sha(root: &Path) -> Result<String> {
    let cmd = PlannedCommand::new("git").arg("rev-parse").arg("HEAD");
    let output = capture_command(root, &cmd)?;
    if !output.status.success() {
        bail!(
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)
        .context("git rev-parse HEAD returned non-UTF-8")?
        .trim()
        .to_string())
}

#[expect(
    clippy::disallowed_methods,
    reason = "fabro-dev opens the CSV file synchronously; bench-tests is a CLI tool, not a Tokio runtime"
)]
fn open_csv_writer(path: &Path) -> Result<csv::Writer<fs::File>> {
    let already_existed = path.exists();
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut writer = csv::WriterBuilder::new()
        .has_headers(false)
        .from_writer(file);
    if !already_existed {
        writer
            .write_record([
                "git_sha",
                "run_index",
                "started_at",
                "binary",
                "package",
                "classname",
                "test_name",
                "status",
                "duration_ms",
            ])
            .with_context(|| format!("writing CSV header to {}", path.display()))?;
    }
    Ok(writer)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestStatus {
    Passed,
    Failed,
    Skipped,
}

impl TestStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JunitCase {
    binary:      String,
    package:     String,
    classname:   String,
    test_name:   String,
    status:      TestStatus,
    duration_ms: u64,
}

struct Totals {
    passed:  usize,
    failed:  usize,
    skipped: usize,
}

impl Totals {
    fn tally(cases: &[JunitCase]) -> Self {
        let mut totals = Self {
            passed:  0,
            failed:  0,
            skipped: 0,
        };
        for case in cases {
            match case.status {
                TestStatus::Passed => totals.passed += 1,
                TestStatus::Failed => totals.failed += 1,
                TestStatus::Skipped => totals.skipped += 1,
            }
        }
        totals
    }
}

impl std::fmt::Display for Totals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "passed={p} failed={fail} skipped={s}",
            p = self.passed,
            fail = self.failed,
            s = self.skipped,
        )
    }
}

fn package_from_binary(binary: &str) -> &str {
    binary.split("::").next().unwrap_or(binary)
}

fn parse_junit(xml: &str) -> Result<Vec<JunitCase>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut cases = Vec::new();
    let mut current_suite: Option<String> = None;
    let mut current_case: Option<PartialCase> = None;
    let mut buf = Vec::new();

    loop {
        match reader
            .read_event_into(&mut buf)
            .context("reading JUnit XML")?
        {
            Event::Start(e) => match e.name().as_ref() {
                b"testsuite" => {
                    current_suite = attr(&e, "name")?;
                }
                b"testcase" => {
                    current_case = Some(parse_testcase_attrs(&e)?);
                }
                b"failure" | b"error" => {
                    if let Some(case) = current_case.as_mut() {
                        case.status = TestStatus::Failed;
                    }
                }
                b"skipped" => {
                    if let Some(case) = current_case.as_mut() {
                        case.status = TestStatus::Skipped;
                    }
                }
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                b"testcase" => {
                    let case = parse_testcase_attrs(&e)?;
                    push_case(&mut cases, current_suite.as_deref(), case);
                }
                b"failure" | b"error" => {
                    if let Some(case) = current_case.as_mut() {
                        case.status = TestStatus::Failed;
                    }
                }
                b"skipped" => {
                    if let Some(case) = current_case.as_mut() {
                        case.status = TestStatus::Skipped;
                    }
                }
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"testcase" => {
                    if let Some(case) = current_case.take() {
                        push_case(&mut cases, current_suite.as_deref(), case);
                    }
                }
                b"testsuite" => {
                    current_suite = None;
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(cases)
}

#[derive(Debug)]
struct PartialCase {
    name:      String,
    classname: String,
    time:      f64,
    status:    TestStatus,
}

fn parse_testcase_attrs(e: &BytesStart<'_>) -> Result<PartialCase> {
    let name = attr(e, "name")?.unwrap_or_default();
    let classname = attr(e, "classname")?.unwrap_or_default();
    let time = attr(e, "time")?
        .as_deref()
        .and_then(|raw| raw.parse::<f64>().ok())
        .unwrap_or(0.0);
    Ok(PartialCase {
        name,
        classname,
        time,
        status: TestStatus::Passed,
    })
}

fn push_case(cases: &mut Vec<JunitCase>, suite: Option<&str>, partial: PartialCase) {
    let binary = suite.unwrap_or("").to_string();
    let package = package_from_binary(&binary).to_string();
    cases.push(JunitCase {
        binary,
        package,
        classname: partial.classname,
        test_name: partial.name,
        status: partial.status,
        duration_ms: seconds_to_ms(partial.time),
    });
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "Test durations are bounded: clamped to [0, u64::MAX-as-f64]; loss of precision past 2^53 ms (~285k years) is irrelevant"
)]
fn seconds_to_ms(seconds: f64) -> u64 {
    let ms = (seconds.max(0.0) * 1000.0).round();
    if !ms.is_finite() || ms <= 0.0 {
        return 0;
    }
    if ms >= u64::MAX as f64 {
        return u64::MAX;
    }
    ms as u64
}

fn attr(e: &BytesStart<'_>, key: &str) -> Result<Option<String>> {
    for attr in e.attributes() {
        let attr = attr.context("reading attribute")?;
        if attr.key.as_ref() == key.as_bytes() {
            let value = attr
                .unescape_value()
                .context("unescaping attribute value")?
                .into_owned();
            return Ok(Some(value));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="4" failures="1" errors="0">
  <testsuite name="fabro-cli::lib" tests="2" failures="0" errors="0" skipped="0">
    <testcase name="adds_two" classname="math::add" time="0.001"/>
    <testcase name="multiplies" classname="math::mul" time="0.0005"/>
  </testsuite>
  <testsuite name="fabro-server$bin/fabro" tests="2" failures="1" errors="0" skipped="1">
    <testcase name="boots" classname="server::boot" time="2.504">
      <failure type="test failure" message="boom"/>
    </testcase>
    <testcase name="ignored_for_now" classname="server::boot" time="0">
      <skipped/>
    </testcase>
  </testsuite>
</testsuites>"#;

    #[test]
    fn parses_passed_failed_and_skipped_cases() {
        let cases = parse_junit(SAMPLE).expect("parse");
        assert_eq!(cases.len(), 4);

        let adds = &cases[0];
        assert_eq!(adds.binary, "fabro-cli::lib");
        assert_eq!(adds.package, "fabro-cli");
        assert_eq!(adds.classname, "math::add");
        assert_eq!(adds.test_name, "adds_two");
        assert_eq!(adds.status, TestStatus::Passed);
        assert_eq!(adds.duration_ms, 1);

        let multiplies = &cases[1];
        assert_eq!(multiplies.duration_ms, 1, "0.0005s rounds to 1ms");

        let boots = &cases[2];
        assert_eq!(boots.binary, "fabro-server$bin/fabro");
        assert_eq!(boots.package, "fabro-server$bin/fabro");
        assert_eq!(boots.test_name, "boots");
        assert_eq!(boots.status, TestStatus::Failed);
        assert_eq!(boots.duration_ms, 2504);

        let ignored = &cases[3];
        assert_eq!(ignored.status, TestStatus::Skipped);
        assert_eq!(ignored.duration_ms, 0);
    }

    #[test]
    fn package_from_binary_strips_target_suffix() {
        assert_eq!(package_from_binary("fabro-cli::lib"), "fabro-cli");
        assert_eq!(package_from_binary("fabro-cli::tests"), "fabro-cli");
        assert_eq!(package_from_binary("fabro-server"), "fabro-server");
        assert_eq!(package_from_binary(""), "");
    }

    #[test]
    fn handles_empty_test_suite() {
        let xml =
            r#"<?xml version="1.0"?><testsuites><testsuite name="empty" tests="0"/></testsuites>"#;
        let cases = parse_junit(xml).expect("parse");
        assert!(cases.is_empty());
    }

    #[test]
    fn totals_tally_counts_each_status() {
        let cases = parse_junit(SAMPLE).expect("parse");
        let totals = Totals::tally(&cases);
        assert_eq!(totals.passed, 2);
        assert_eq!(totals.failed, 1);
        assert_eq!(totals.skipped, 1);
    }
}
