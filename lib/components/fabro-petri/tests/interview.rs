//! Petri's human gates through Fabro's interview adapter: a question is
//! posted as Fabro's `interview.started` under Petri's own id and the
//! projection's stage label, the answer submitted to the control
//! interviewer under that id reaches the gate, two parallel gates each get
//! their own answer, an expired question is completed as a
//! timeout with the gate's default, an auto-approved run answers itself,
//! and a cancelled run interrupts its question.
//!
//! Every run takes its host scope through the sandbox-driver host plugin,
//! so the tests skip, and say why, when the executable is not found,
//! unless `FABRO_REQUIRE_SANDBOX_PLUGINS` is set.

mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};

use fabro_interview::{Answer as LegacyAnswer, ControlInterviewer};
use fabro_petri::check::Launch;
use fabro_petri::engine::{self, RunStatus};
use fabro_petri::interview::{
    Approval, AskedQuestion, FabroInterviewer, QuestionNotice, QuestionSink, engine_submission,
};
use fabro_petri::runtime::RuntimeSpec;
use fabro_types::{Principal, QuestionType, SystemActorKind};
use petri_execution::{Delivery, InterviewReceipt, RECEIPT_FILE, ReplyRecord};
use petri_store::MemoryRunStore;
use support::{SETTINGS, admit, all_records, host_plugin, run_request, wait_until};
use tokio::fs;

/// A board of every notice the adapter posted.
#[derive(Default)]
struct Board {
    notices: Mutex<Vec<QuestionNotice>>,
}

impl Board {
    fn notices(&self) -> Vec<QuestionNotice> {
        self.notices.lock().expect("not poisoned").clone()
    }

    /// Whether a notice other than `Asked` names `question_id`.
    fn ended(&self, question_id: &str) -> bool {
        self.notices().iter().any(|notice| match notice {
            QuestionNotice::Asked(_) => false,
            QuestionNotice::Answered {
                question_id: id, ..
            }
            | QuestionNotice::Expired {
                question_id: id, ..
            }
            | QuestionNotice::Interrupted {
                question_id: id, ..
            } => id == question_id,
        })
    }

    /// The notices once the end of `question_id` is posted, which lands on
    /// a task of its own.
    async fn wait_ended(&self, question_id: &str) -> Vec<QuestionNotice> {
        wait_until(&format!("`{question_id}` to end"), || {
            self.ended(question_id)
        })
        .await;
        self.notices()
    }

    fn asked(&self, stage: &str) -> Option<AskedQuestion> {
        self.notices().into_iter().find_map(|notice| match notice {
            QuestionNotice::Asked(asked) if asked.stage == stage => Some(asked),
            _ => None,
        })
    }

    /// The question `stage` asked, once it is posted.
    async fn wait_asked(&self, stage: &str) -> AskedQuestion {
        wait_until(&format!("`{stage}` to ask"), || self.asked(stage).is_some()).await;
        self.asked(stage).expect("asked")
    }
}

#[async_trait::async_trait]
impl QuestionSink for Board {
    async fn post(&self, notice: QuestionNotice) -> anyhow::Result<()> {
        self.notices.lock().expect("not poisoned").push(notice);
        Ok(())
    }
}

/// One yes/no gate whose branches leave a marker file each.
fn one_gate(markers: &Path, gate_attrs: &str) -> String {
    format!(
        r#"digraph G {{
    start [shape=Mdiamond]
    exit [shape=Msquare]
    gate [shape=hexagon, label="Go?", question_type="yes_no"{gate_attrs}]
    yes [shape=parallelogram, script="touch {dir}/yes"]
    no [shape=parallelogram, script="touch {dir}/no"]
    start -> gate
    gate -> yes [label="[Y] Yes"]
    gate -> no [label="[N] No"]
    yes -> exit
    no -> exit
}}"#,
        dir = markers.display()
    )
}

/// Two gates as the branches of one parallel node; the join's results are
/// written out, so each gate's answer is read from its branch result.
fn two_gates(markers: &Path) -> String {
    format!(
        r#"digraph G {{
    start [shape=Mdiamond]
    exit [shape=Msquare]
    fan [shape=component]
    a [shape=hexagon, label="A?", question_type="yes_no"]
    b [shape=hexagon, label="B?", question_type="yes_no"]
    join [shape=tripleoctagon]
    report [shape=parallelogram, script="cat > {dir}/results.json", stdin_source="context.parallel.results"]
    start -> fan
    fan -> a
    fan -> b
    a -> join [label="[Y] Yes"]
    a -> join [label="[N] No"]
    b -> join [label="[Y] Yes"]
    b -> join [label="[N] No"]
    join -> report -> exit
}}"#,
        dir = markers.display()
    )
}

struct Gate {
    _root:   tempfile::TempDir,
    markers: std::path::PathBuf,
    run_dir: std::path::PathBuf,
    store:   Arc<MemoryRunStore>,
    control: Arc<ControlInterviewer>,
    board:   Arc<Board>,
}

impl Gate {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("a temp dir");
        let markers = root.path().join("markers");
        std::fs::create_dir_all(&markers).expect("the marker dir creates");
        Self {
            run_dir: root.path().join("run"),
            markers,
            _root: root,
            store: Arc::new(MemoryRunStore::new()),
            control: Arc::new(ControlInterviewer::new()),
            board: Arc::new(Board::default()),
        }
    }

    fn interviewer(&self, approval: Approval) -> FabroInterviewer {
        FabroInterviewer::new(Arc::clone(&self.control), approval).with_sink(self.board.clone())
    }

    fn marker(&self, name: &str) -> bool {
        self.markers.join(name).exists()
    }

    async fn receipt(&self) -> InterviewReceipt {
        let text = fs::read_to_string(self.run_dir.join(RECEIPT_FILE))
            .await
            .expect("the receipt was written");
        serde_json::from_str(&text).expect("the receipt parses")
    }
}

/// The question is posted with Fabro's type, options and stage; the answer
/// submitted under the posted id, as the API delivers it, routes the gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gate_answered_under_the_posted_id_routes_on_the_answer() {
    if host_plugin().is_none() {
        return;
    }
    let gate = Gate::new();
    let workflow = one_gate(&gate.markers, "");
    let runtime = RuntimeSpec::default();
    let graphs = admit(
        &[("workflow.fabro", &workflow), ("workflow.toml", SETTINGS)],
        Launch::default(),
        &runtime,
    );
    let request = run_request(
        "gate",
        &gate.run_dir,
        graphs,
        gate.store.clone(),
        runtime,
        gate.interviewer(Approval::Prompt),
    );
    let answer = {
        let board = gate.board.clone();
        let control = gate.control.clone();
        tokio::spawn(async move {
            let asked = board.wait_asked("gate@1").await;
            control
                .submit(&asked.question_id, engine_submission(LegacyAnswer::no()))
                .await
                .expect("the answer is accepted");
            asked
        })
    };

    let outcome = engine::run(request).await.expect("the run ends");
    let asked = answer.await.expect("the answer task ends");

    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(
        gate.marker("no") && !gate.marker("yes"),
        "the no branch ran"
    );
    assert_eq!(
        asked.stage, "gate@1",
        "the projection's label for the firing"
    );
    assert_eq!(asked.text, "Go?");
    assert_eq!(asked.question_type, QuestionType::YesNo);
    assert_eq!(
        asked
            .options
            .iter()
            .map(|option| (option.key.as_str(), option.label.as_str()))
            .collect::<Vec<_>>(),
        vec![("Y", "[Y] Yes"), ("N", "[N] No")]
    );
    assert!(
        asked.question_id.starts_with("gate#"),
        "Petri's id, as the projection serves it: {}",
        asked.question_id
    );
    assert_eq!(asked.identity.node, "gate");
    assert_eq!(asked.identity.execution, 0);
    assert_eq!(asked.identity.occurrence, 1);
    assert_eq!(asked.identity.ask, 1);
    assert_eq!(asked.identity.invocation_path, "/");
    let notices = gate.board.notices();
    assert!(
        matches!(
            &notices[1],
            QuestionNotice::Answered { question_id, answer, actor: Principal::System { system_kind: SystemActorKind::Engine }, .. }
                if *question_id == asked.question_id && answer == "N"
        ),
        "{notices:?}"
    );
    let receipt = gate.receipt().await;
    assert!(receipt.is_clean(), "{:?}", receipt.errors);
    assert_eq!(receipt.questions.len(), 1);
    assert_eq!(receipt.questions[0].delivery, Delivery::Delivered);
    assert_eq!(receipt.questions[0].reply, ReplyRecord::Answered {
        choice:  Some("N".to_string()),
        choices: Vec::new(),
        text:    None,
    });
}

/// Two branches ask at once; each answer, submitted under its own id in
/// the other order, lands on its own branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_parallel_gates_each_bind_their_own_answer() {
    if host_plugin().is_none() {
        return;
    }
    let gate = Gate::new();
    let workflow = two_gates(&gate.markers);
    let runtime = RuntimeSpec::default();
    let graphs = admit(
        &[("workflow.fabro", &workflow), ("workflow.toml", SETTINGS)],
        Launch::default(),
        &runtime,
    );
    let request = run_request(
        "gates",
        &gate.run_dir,
        graphs,
        gate.store.clone(),
        runtime,
        gate.interviewer(Approval::Prompt),
    );
    let answers = {
        let board = gate.board.clone();
        let control = gate.control.clone();
        tokio::spawn(async move {
            // Both are pending before either is answered, and `b` first.
            let a = board.wait_asked("a@1").await;
            let b = board.wait_asked("b@1").await;
            assert_ne!(a.question_id, b.question_id);
            control
                .submit(&b.question_id, engine_submission(LegacyAnswer::yes()))
                .await
                .expect("b's answer is accepted");
            control
                .submit(&a.question_id, engine_submission(LegacyAnswer::no()))
                .await
                .expect("a's answer is accepted");
            (a, b)
        })
    };

    let outcome = engine::run(request).await.expect("the run ends");
    let (a, b) = answers.await.expect("the answer task ends");

    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    let results: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(gate.markers.join("results.json"))
            .await
            .expect("the join wrote its results"),
    )
    .expect("the results parse");
    let results = results.as_array().expect("a list of branch results");
    assert_eq!(results.len(), 2, "{results:?}");
    assert_eq!(results[0]["id"], "a");
    assert_eq!(results[0]["context_updates"]["human.gate.selected"], "N");
    assert_eq!(results[1]["id"], "b");
    assert_eq!(results[1]["context_updates"]["human.gate.selected"], "Y");
    assert!(
        a.identity.invocation_path.starts_with("/branch:"),
        "{}",
        a.identity.invocation_path
    );
    assert_ne!(a.identity.invocation_path, b.identity.invocation_path);
    let receipt = gate.receipt().await;
    assert!(receipt.is_clean(), "{:?}", receipt.errors);
    assert_eq!(receipt.questions.len(), 2);
}

/// The gate's deadline passes with no answer: the adapter completes the
/// question as a timeout, the receipt says the gate took its default, and
/// the default's branch runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unanswered_question_expires_with_the_gates_default() {
    if host_plugin().is_none() {
        return;
    }
    let gate = Gate::new();
    let workflow = one_gate(
        &gate.markers,
        r#", timeout="300ms", human.default_choice="no""#,
    );
    let runtime = RuntimeSpec::default();
    let graphs = admit(
        &[("workflow.fabro", &workflow), ("workflow.toml", SETTINGS)],
        Launch::default(),
        &runtime,
    );
    let request = run_request(
        "expiry",
        &gate.run_dir,
        graphs,
        gate.store.clone(),
        runtime,
        gate.interviewer(Approval::Prompt),
    );

    let outcome = engine::run(request).await.expect("the run ends");

    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(gate.marker("no") && !gate.marker("yes"), "the default ran");
    let asked = gate.board.asked("gate@1").expect("asked");
    let notices = gate.board.wait_ended(&asked.question_id).await;
    assert_eq!(asked.timeout_seconds, Some(0.3));
    assert!(
        matches!(
            &notices[1],
            QuestionNotice::Expired { question_id, stage, .. }
                if *question_id == asked.question_id && stage == "gate@1"
        ),
        "{notices:?}"
    );
    let receipt = gate.receipt().await;
    assert!(receipt.is_clean(), "{:?}", receipt.errors);
    assert_eq!(receipt.questions[0].reply, ReplyRecord::TimedOut {
        default: Some("N".to_string()),
    });
    assert_eq!(receipt.questions[0].delivery, Delivery::Expired);
}

/// An auto-approved run answers its gate at once, attributed to the
/// engine, and still posts the question and its answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_auto_approved_run_answers_yes_at_once() {
    if host_plugin().is_none() {
        return;
    }
    let gate = Gate::new();
    let workflow = one_gate(&gate.markers, "");
    let runtime = RuntimeSpec::default();
    let graphs = admit(
        &[("workflow.fabro", &workflow), ("workflow.toml", SETTINGS)],
        Launch::default(),
        &runtime,
    );
    let request = run_request(
        "auto",
        &gate.run_dir,
        graphs,
        gate.store.clone(),
        runtime,
        gate.interviewer(Approval::Auto),
    );

    let outcome = engine::run(request).await.expect("the run ends");

    assert_eq!(outcome.status, RunStatus::Success, "{outcome:?}");
    assert!(
        gate.marker("yes") && !gate.marker("no"),
        "the yes branch ran"
    );
    let notices = gate.board.notices();
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(
        matches!(
            &notices[1],
            QuestionNotice::Answered { answer, actor: Principal::System { system_kind: SystemActorKind::Engine }, .. }
                if answer == "Y"
        ),
        "{notices:?}"
    );
}

/// A run cancelled while its gate waits: the adapter returns promptly, the
/// question is interrupted, the gate fails closed and the run is cancelled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_run_interrupts_its_pending_question() {
    if host_plugin().is_none() {
        return;
    }
    let gate = Gate::new();
    let workflow = one_gate(&gate.markers, "");
    let runtime = RuntimeSpec::default();
    let graphs = admit(
        &[("workflow.fabro", &workflow), ("workflow.toml", SETTINGS)],
        Launch::default(),
        &runtime,
    );
    let request = run_request(
        "cancel",
        &gate.run_dir,
        graphs,
        gate.store.clone(),
        runtime,
        gate.interviewer(Approval::Prompt),
    );
    let cancel = request.cancel.clone();
    let canceller = {
        let board = gate.board.clone();
        tokio::spawn(async move {
            board.wait_asked("gate@1").await;
            cancel.cancel();
        })
    };

    let outcome = engine::run(request).await.expect("the run ends");
    canceller.await.expect("the cancel task ends");

    assert_eq!(outcome.status, RunStatus::Cancelled, "{outcome:?}");
    assert!(!gate.marker("yes") && !gate.marker("no"), "no branch ran");
    let asked = gate.board.asked("gate@1").expect("asked");
    let notices = gate.board.wait_ended(&asked.question_id).await;
    assert!(
        matches!(
            &notices[1],
            QuestionNotice::Interrupted { reason, .. } if reason == "cancelled"
        ),
        "{notices:?}"
    );
    let receipt = gate.receipt().await;
    assert_eq!(receipt.questions[0].reply, ReplyRecord::Cancelled);
    // Nothing the adapter posted names the answer a person never gave.
    let records = all_records(gate.store.as_ref(), "cancel").await;
    assert!(!records.is_empty());
}
