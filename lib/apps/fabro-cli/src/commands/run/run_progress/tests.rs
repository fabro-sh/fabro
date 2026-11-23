#![expect(
    clippy::disallowed_types,
    reason = "capture the synchronous progress renderer's output in memory"
)]

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use fabro_types::{
    DirtyStatus, ForkSourceRef, GitContext, GitRunTarget, RunStreamItemKind, fixtures, test_support,
};
use fabro_util::terminal::Styles;
use indicatif::{InMemoryTerm, ProgressDrawTarget};
use serde_json::{Value, json};

use super::*;
use crate::commands::run::petri_stream::{self, PrettyState};

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Write for Output {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Output {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

fn visit(seq: u64, event: &str, derived: Value) -> RunStreamItem {
    let mut derived = derived;
    derived["event"] = json!(event);
    RunStreamItem {
        run_id:      fixtures::RUN_1,
        stream_seq:  seq,
        kind:        RunStreamItemKind::Petri,
        id:          seq.to_string(),
        recorded_at: 1_789_706_579_000 + seq * 1000,
        item:        json!({
            "origin": "derived",
            "context": {"invocation": 0, "execution": 0},
            "subject": {
                "node": {"id": 2, "name": "work", "kind": "attractor/x",
                    "meta": {"label": "work", "kind": "agent"}},
                "firing": 2, "visit": 1, "attempt": 1,
                "generation": 0, "branch": {"role": "none"}
            },
            "derived": derived
        }),
    }
}

fn completed(status: &str, executed: bool) -> RunStreamItem {
    visit(
        4,
        "visit.completed",
        json!({
            "outcome": {"status": status},
            "executed": executed,
            "attempts": 1
        }),
    )
}

#[test]
fn progress_and_events_present_stage_outcomes_consistently() {
    let mut cases = Vec::new();
    for (status, executed) in [
        ("skipped", true),
        ("success", false),
        ("success", true),
        ("partial_success", true),
        ("failure", true),
        ("cancelled", true),
        ("unrecognized", true),
    ] {
        let started = visit(1, "visit.started", json!({}));
        let completed = completed(status, executed);
        let raw_before = petri_stream::raw_line(&completed).unwrap();
        let mut progress = Vec::new();
        for verbose in [false, true] {
            let output = Output::default();
            let renderer = ProgressRenderer::new_plain(Box::new(output.clone()), false);
            let mut ui = ProgressUI::with_renderer(renderer, verbose);
            ui.handle_stream_item(&started);
            ui.handle_stream_item(&completed);
            assert!(ui.stage.active_stages.is_empty());
            ui.finish();
            progress.push(output.text());
        }
        let mut state = PrettyState::default();
        let styles = Styles::new(false);
        petri_stream::format_pretty(&started, &styles, &mut state);
        let pretty = petri_stream::format_pretty(&completed, &styles, &mut state).unwrap();
        // Presentation must not change the machine-readable event or turn a
        // stage outcome into the run's terminal result.
        assert_eq!(petri_stream::raw_line(&completed).unwrap(), raw_before);
        assert_eq!(petri_stream::exit_code_of(&completed), None);
        cases.push(json!({
            "status": status, "executed": executed,
            "progress": progress[0], "verbose_progress": progress[1], "events": pretty
        }));
    }
    insta::assert_snapshot!(serde_json::to_string_pretty(&cases).unwrap(), @r#"
    [
      {
        "status": "skipped",
        "executed": true,
        "progress": "    ⊘ work  skipped  3s\n",
        "verbose_progress": "    ⊘ work  skipped  3s\n",
        "events": "04:43:03 ⊘ work  skipped"
      },
      {
        "status": "success",
        "executed": false,
        "progress": "    ⊘ work  skipped  3s\n",
        "verbose_progress": "    ⊘ work  skipped  3s\n",
        "events": "04:43:03 ⊘ work  skipped"
      },
      {
        "status": "success",
        "executed": true,
        "progress": "    ✓ work  3s\n",
        "verbose_progress": "    ✓ work  3s\n",
        "events": "04:43:03 ✓ work  3s"
      },
      {
        "status": "partial_success",
        "executed": true,
        "progress": "    ✓ work  partial  3s\n",
        "verbose_progress": "    ✓ work  partial  3s\n",
        "events": "04:43:03 ✓ work  partial  3s"
      },
      {
        "status": "failure",
        "executed": true,
        "progress": "    ✗ work\n    Error: failure\n",
        "verbose_progress": "    ✗ work\n    Error: failure\n",
        "events": "04:43:03 ✗ work  failure"
      },
      {
        "status": "cancelled",
        "executed": true,
        "progress": "    ✗ work  3s\n",
        "verbose_progress": "    ✗ work  3s\n",
        "events": "04:43:03 ✗ work  cancelled"
      },
      {
        "status": "unrecognized",
        "executed": true,
        "progress": "    ✗ work\n    Error: unrecognized\n",
        "verbose_progress": "    ✗ work\n    Error: unrecognized\n",
        "events": "04:43:03 ✗ work  unrecognized"
      }
    ]
    "#);
}

#[test]
fn skipped_stage_finishes_terminal_progress_with_a_neutral_label() {
    for (status, executed) in [("skipped", true), ("success", false)] {
        for verbose in [false, true] {
            let renderer = ProgressRenderer::new_tty();
            renderer.hide();
            let mut ui = ProgressUI::with_renderer(renderer, verbose);
            ui.handle_stream_item(&visit(1, "visit.started", json!({})));
            let bar = ui.stage.active_stages["work"].spinner.clone();
            ui.handle_stream_item(&completed(status, executed));
            assert_eq!(console::strip_ansi_codes(&bar.message()), "⊘ work");
            assert_eq!(console::strip_ansi_codes(&bar.prefix()), "skipped  3s");
            assert!(bar.is_finished());
            assert!(ui.stage.active_stages.is_empty());
            ui.finish();
        }
    }
}

#[test]
fn skipped_stage_is_dim_in_both_renderers_and_visible_in_plain_replay_without_a_start() {
    let completed = completed("skipped", true);
    let output = Output::default();
    let renderer = ProgressRenderer::new_plain(Box::new(output.clone()), true);
    let mut ui = ProgressUI::with_renderer(renderer, false);
    ui.handle_stream_item(&completed);
    ui.finish();
    let progress = output.text();
    let pretty =
        petri_stream::format_pretty(&completed, &Styles::new(true), &mut PrettyState::default())
            .unwrap();
    for text in [&progress, &pretty] {
        assert!(text.contains("\u{1b}[2m⊘\u{1b}[0m"), "{text:?}");
        assert!(text.contains("\u{1b}[2mskipped\u{1b}[0m"), "{text:?}");
        assert!(!text.contains('✗'), "{text:?}");
    }
    assert_eq!(
        console::strip_ansi_codes(&progress),
        "    ⊘ work  skipped  0ms\n"
    );
}

fn branch_record(base_sha: Option<&str>) -> RunStreamItem {
    RunStreamItem {
        run_id:      fixtures::RUN_1,
        stream_seq:  2,
        kind:        RunStreamItemKind::Platform,
        id:          "2".to_string(),
        recorded_at: 1_789_706_579_000,
        item:        json!({"record": {
            "kind": "run.branch",
            "run_branch": format!("fabro/run/{}", fixtures::RUN_1),
            "base_sha": base_sha
        }}),
    }
}

fn source_spec() -> RunSpec {
    RunSpec {
        git: Some(GitContext {
            origin_url: String::new(),
            branch:     "main".to_string(),
            // The branch-point record, not this creation-time observation,
            // supplies the SHA displayed next to the source branch.
            sha:        Some("1111111111111111111111111111111111111111".to_string()),
            dirty:      DirtyStatus::Clean,
        }),
        ..test_support::test_run_spec()
    }
}

fn render_base(spec: &RunSpec, base_sha: Option<&str>, verbose: bool) -> String {
    let output = Output::default();
    let renderer = ProgressRenderer::new_plain(Box::new(output.clone()), false);
    let mut ui = ProgressUI::with_renderer(renderer, verbose).with_source_spec(spec);
    let record = branch_record(base_sha);
    let raw = petri_stream::raw_line(&record).unwrap();
    ui.handle_stream_item(&record);
    ui.finish();
    assert_eq!(petri_stream::raw_line(&record).unwrap(), raw);
    output.text()
}

#[test]
fn base_header_uses_saved_source_branch_and_recorded_base_commit() {
    let spec = source_spec();
    for verbose in [false, true] {
        insta::allow_duplicates! {
            insta::assert_snapshot!(render_base(&spec, Some("abcdef0123456789"), verbose), @"    Base: main (abcdef012345)");
        }
        assert_eq!(render_base(&spec, None, verbose), "");
    }
}

#[test]
fn base_header_uses_only_the_commit_when_source_branch_is_unknown_or_ambiguous() {
    let mut absent = source_spec();
    absent.git = None;
    let mut empty = source_spec();
    empty.git.as_mut().unwrap().branch = "  ".to_string();
    let mut fork = source_spec();
    fork.fork_source_ref = Some(ForkSourceRef {
        source_run_id:  fixtures::RUN_2,
        checkpoint_sha: "abcdef0123456789".to_string(),
    });
    let mut tag = source_spec();
    tag.target = Some(RunTarget::Git(GitRunTarget {
        repo:   "acme/widgets".to_string(),
        branch: "main".to_string(),
        tag:    Some("v1".to_string()),
        sha:    None,
    }));
    for spec in [absent, empty, fork, tag] {
        for verbose in [false, true] {
            insta::allow_duplicates! {
                insta::assert_snapshot!(render_base(&spec, Some("abcdef0123456789"), verbose), @"    Base: abcdef012345");
            }
        }
    }
}

fn applied_route(from: &str, to: &str, label: &Value, back: bool) -> RunStreamItem {
    RunStreamItem {
        run_id:      fixtures::RUN_1,
        stream_seq:  1,
        kind:        RunStreamItemKind::Petri,
        id:          "execution 0/19/0".to_string(),
        recorded_at: 0,
        item:        json!({
            "origin": "core",
            "context": {"invocation": 0, "execution": 0},
            "subject": {"node": {"name": from, "meta": {"edges": {
                "0": {"to": to, "label": "Not selected"},
                "7": {"to": to, "label": label}
            }}}, "firing": 2},
            "record": {"seq": 19, "body": {"event": "route.applied",
                "kind": "edge", "firing": 2, "group": 0, "edge": 7}},
            "derived": {"target": {"name": to}, "transition": "Continue", "back": back}
        }),
    }
}

fn route_progress(item: &RunStreamItem, verbose: bool) -> String {
    let output = Output::default();
    let renderer = ProgressRenderer::new_plain(Box::new(output.clone()), false);
    let mut ui = ProgressUI::with_renderer(renderer, verbose);
    ui.handle_stream_item(&route_visit(item, "visit.started", None));
    ui.handle_stream_item(&route_visit(item, "visit.completed", None));
    output.0.lock().unwrap().clear();
    ui.handle_stream_item(item);
    output.text()
}

fn route_visit(route: &RunStreamItem, event: &str, label: Option<&str>) -> RunStreamItem {
    let mut item = visit(
        0,
        event,
        json!({"outcome": {"status": "success"}, "executed": true}),
    );
    item.item["subject"] = route.item["subject"].clone();
    item.item["subject"]["node"]["meta"]["kind"] = json!("agent");
    item.item["subject"]["visit"] = json!(1);
    if let Some(label) = label {
        item.item["subject"]["node"]["meta"]["label"] = json!(label);
    }
    item
}

#[test]
fn selected_route_labels_render_beneath_stages_and_in_pretty_events() {
    let mut transcript = String::new();
    for (from, to, label, back) in [
        (
            "Run tests",
            "Open pull request",
            json!("Tests passed"),
            false,
        ),
        (
            "Review changes",
            "Revise implementation",
            json!("Changes requested"),
            false,
        ),
        ("Approval", "Deploy", json!("Approved"), false),
        ("Verify fix", "Investigate", json!("Still failing"), true),
        ("work", "a", json!("yes"), false),
        ("start", "work", Value::Null, false),
        ("verify", "work", Value::Null, true),
    ] {
        let item = applied_route(from, to, &label, back);
        let before = petri_stream::raw_line(&item).unwrap();
        transcript.push_str(&route_progress(&item, true));
        transcript.push_str(
            &petri_stream::format_pretty(&item, &Styles::new(false), &mut PrettyState::default())
                .unwrap(),
        );
        transcript.push('\n');
        assert_eq!(route_progress(&item, false), "");
        assert_eq!(petri_stream::raw_line(&item).unwrap(), before);
    }
    insta::assert_snapshot!(transcript, @r#"
            └─ Tests passed
    00:00:00    Run tests → Open pull request  [Tests passed]
            └─ Changes requested
    00:00:00    Review changes → Revise implementation  [Changes requested]
            └─ Approved
    00:00:00    Approval → Deploy  [Approved]
            ↺ Still failing
    00:00:00    Verify fix ↺ Investigate  [Still failing]
            └─ yes
    00:00:00    work → a  [yes]
        → start → work
    00:00:00    start → work continue
        ↺ verify → work  (loop restart)
    00:00:00    verify → work continue (loop)
    "#);
}

#[test]
fn selected_route_labels_coexist_with_transition_and_condition_details() {
    let mut item = applied_route("verify", "finish", &json!("Tests passed"), false);
    item.item["subject"]["node"]["meta"]["edges"]["7"]["condition"] = json!("outcome=succeeded");
    item.item["derived"]["transition"] = json!("Restart");
    let pretty =
        petri_stream::format_pretty(&item, &Styles::new(false), &mut PrettyState::default())
            .unwrap();
    insta::assert_snapshot!(format!("{}{pretty}", route_progress(&item, true)), @r#"
            └─ Tests passed  [restart]
    00:00:00    verify → finish  [Tests passed] restart when outcome=succeeded
    "#);
}

#[test]
fn unavailable_selected_route_labels_preserve_unlabeled_output() {
    let baseline = applied_route("work", "next", &Value::Null, false);
    let mut cases = vec![
        applied_route("work", "next", &json!(""), false),
        applied_route("work", "next", &json!(42), false),
    ];
    let mut missing_edge = baseline.clone();
    missing_edge.item["record"]["body"]["edge"] = json!(99);
    cases.push(missing_edge);
    let mut missing_metadata = baseline.clone();
    missing_metadata.item["subject"]["node"]["meta"] = json!({});
    cases.push(missing_metadata);
    let mut no_edge = baseline.clone();
    no_edge.item["record"]["body"] = json!({"event": "route.applied", "kind": "jump"});
    cases.push(no_edge);
    for item in cases {
        assert_eq!(route_progress(&item, true), route_progress(&baseline, true));
        assert_eq!(
            petri_stream::format_pretty(&item, &Styles::new(false), &mut PrettyState::default()),
            petri_stream::format_pretty(
                &baseline,
                &Styles::new(false),
                &mut PrettyState::default()
            ),
        );
    }
}

#[test]
fn selected_route_labels_escape_controls_and_preserve_unicode() {
    let item = applied_route(
        "review",
        "revise",
        &json!("Réessayer \"tests\"\n\u{1b}[31m"),
        false,
    );
    insta::assert_snapshot!(route_progress(&item, true), @r#"
    └─ Réessayer \"tests\"\n\u{1b}[31m
    "#);
}

fn review_route(back: bool) -> RunStreamItem {
    applied_route(
        "review_images",
        if back { "render_images" } else { "render_html" },
        &json!(if back {
            "Regenerate images"
        } else {
            "Keep images"
        }),
        back,
    )
}

fn target_route(source: &RunStreamItem) -> RunStreamItem {
    let target = source.item["derived"]["target"]["name"].as_str().unwrap();
    let mut route = applied_route(target, "exit", &Value::Null, false);
    route.item["subject"]["firing"] = json!(3);
    route.item["subject"]["node"]["meta"]["edges"] = json!({});
    route
}

#[test]
fn decision_lines_follow_display_labels_or_node_names_in_plain_progress() {
    let mut transcript = String::new();
    for back in [false, true] {
        for labels in [true, false] {
            let output = Output::default();
            let renderer = ProgressRenderer::new_plain(Box::new(output.clone()), false);
            let mut ui = ProgressUI::with_renderer(renderer, true);
            let source = review_route(back);
            let target = target_route(&source);
            let source_label = labels.then_some("Review images");
            let target_label = labels.then_some(if back {
                "Render 12 images"
            } else {
                "Render HTML reading"
            });
            ui.handle_stream_item(&route_visit(&source, "visit.started", source_label));
            ui.handle_stream_item(&route_visit(&source, "visit.completed", source_label));
            // The route follows the target's start in Petri's stream.
            ui.handle_stream_item(&route_visit(&target, "visit.started", target_label));
            ui.handle_stream_item(&source);
            ui.handle_stream_item(&route_visit(&target, "visit.completed", target_label));
            ui.finish();
            transcript.push_str(&output.text());
        }
    }
    insta::assert_snapshot!(transcript, @r#"
        ✓ Review images  0ms
            └─ Keep images
        ✓ Render HTML reading  0ms
        ✓ review_images  0ms
            └─ Keep images
        ✓ render_html  0ms
        ✓ Review images  0ms
            ↺ Regenerate images
        ✓ Render 12 images  0ms
        ✓ review_images  0ms
            ↺ Regenerate images
        ✓ render_images  0ms
    "#);
}

#[test]
fn tty_decision_stays_with_source_when_another_stage_has_already_finished() {
    for back in [false, true] {
        let term = InMemoryTerm::new(24, 100);
        let renderer = ProgressRenderer::with_draw_target(ProgressDrawTarget::term_like(Box::new(
            term.clone(),
        )));
        let mut ui = ProgressUI::with_renderer(renderer, true);
        let source = review_route(back);
        let target = target_route(&source);
        ui.handle_stream_item(&route_visit(
            &source,
            "visit.started",
            Some("Review images"),
        ));
        ui.handle_stream_item(&route_visit(
            &source,
            "visit.completed",
            Some("Review images"),
        ));
        ui.handle_stream_item(&route_visit(&target, "visit.started", Some("Next stage")));
        ui.handle_stream_item(&route_visit(&target, "visit.completed", Some("Next stage")));
        ui.handle_stream_item(&source);
        ui.finish();
        let screen = term.contents();
        let decision = if back {
            "↺ Regenerate images"
        } else {
            "└─ Keep images"
        };
        let lines: Vec<_> = screen
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        assert_eq!(lines.len(), 3, "{screen}");
        assert!(lines[0].contains("✓ Review images"), "{screen}");
        assert_eq!(lines[1], format!("        {decision}"));
        assert!(lines[2].contains("✓ Next stage"), "{screen}");
        assert!(!screen.contains("review_images"));
    }
}

#[test]
fn detached_or_interleaved_plain_routes_keep_context() {
    for with_source in [false, true] {
        let output = Output::default();
        let renderer = ProgressRenderer::new_plain(Box::new(output.clone()), false);
        let mut ui = ProgressUI::with_renderer(renderer, true);
        let source = review_route(false);
        if with_source {
            ui.handle_stream_item(&route_visit(
                &source,
                "visit.started",
                Some("Review images"),
            ));
            ui.handle_stream_item(&route_visit(
                &source,
                "visit.completed",
                Some("Review images"),
            ));
            // An append-only transcript cannot move the decision above this line.
            ui.renderer.print_line(4, "Other activity");
        }
        output.0.lock().unwrap().clear();
        ui.handle_stream_item(&source);
        insta::allow_duplicates! {
            insta::assert_snapshot!(output.text(), @"    → review_images → render_html  [Keep images]");
        }
    }
}

#[test]
fn tty_self_loop_decisions_attach_to_the_correct_visit() {
    let term = InMemoryTerm::new(24, 100);
    let renderer =
        ProgressRenderer::with_draw_target(ProgressDrawTarget::term_like(Box::new(term.clone())));
    let mut ui = ProgressUI::with_renderer(renderer, true);
    let first = applied_route("review", "review", &json!("Regenerate images"), true);
    let mut second = applied_route("review", "exit", &json!("Keep images"), false);
    second.item["subject"]["firing"] = json!(3);
    for (route, name) in [(&first, "First review"), (&second, "Second review")] {
        ui.handle_stream_item(&route_visit(route, "visit.started", Some(name)));
        ui.handle_stream_item(&route_visit(route, "visit.completed", Some(name)));
    }
    // Both completed visits share a node name; routes arrive in reverse order.
    ui.handle_stream_item(&second);
    ui.handle_stream_item(&first);
    ui.finish();
    let screen = term.contents();
    let lines: Vec<_> = screen
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 4, "{screen}");
    assert!(lines[0].contains("✓ First review"), "{screen}");
    assert_eq!(lines[1], "        ↺ Regenerate images");
    assert!(lines[2].contains("✓ Second review"), "{screen}");
    assert_eq!(lines[3], "        └─ Keep images");
}
