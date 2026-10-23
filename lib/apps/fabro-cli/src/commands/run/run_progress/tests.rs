#![expect(
    clippy::disallowed_types,
    reason = "capture the synchronous progress renderer's output in memory"
)]

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use fabro_types::{RunStreamItemKind, fixtures};
use fabro_util::terminal::Styles;
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
