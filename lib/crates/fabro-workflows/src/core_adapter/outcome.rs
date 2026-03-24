use fabro_core::outcome::{
    FailureDetail as CoreFailureDetail, Outcome as CoreOutcome, StageStatus as CoreStatus,
};

use crate::error::{classify_failure_reason, FailureClass};
use crate::outcome::{
    FailureDetail as WfFailureDetail, Outcome as WfOutcome, StageStatus as WfStatus,
};

pub fn wf_to_core_status(s: &WfStatus) -> CoreStatus {
    match s {
        WfStatus::Success => CoreStatus::Success,
        WfStatus::Fail => CoreStatus::Fail,
        WfStatus::Skipped => CoreStatus::Skipped,
        WfStatus::PartialSuccess => CoreStatus::PartialSuccess,
        WfStatus::Retry => CoreStatus::Retry,
    }
}

pub fn core_to_wf_status(s: &CoreStatus) -> WfStatus {
    match s {
        CoreStatus::Success => WfStatus::Success,
        CoreStatus::Fail => WfStatus::Fail,
        CoreStatus::Skipped => WfStatus::Skipped,
        CoreStatus::PartialSuccess => WfStatus::PartialSuccess,
        CoreStatus::Retry => WfStatus::Retry,
    }
}

pub fn wf_to_core_outcome(wf: &WfOutcome) -> CoreOutcome {
    CoreOutcome {
        status: wf_to_core_status(&wf.status),
        preferred_label: wf.preferred_label.clone(),
        suggested_next_ids: wf.suggested_next_ids.clone(),
        context_updates: wf.context_updates.clone(),
        jump_to_node: wf.jump_to_node.clone(),
        notes: wf.notes.clone(),
        failure: wf.failure.as_ref().map(wf_to_core_failure),
        metadata: Default::default(),
    }
}

pub fn core_to_wf_outcome(core: &CoreOutcome) -> WfOutcome {
    WfOutcome {
        status: core_to_wf_status(&core.status),
        preferred_label: core.preferred_label.clone(),
        suggested_next_ids: core.suggested_next_ids.clone(),
        context_updates: core.context_updates.clone(),
        jump_to_node: core.jump_to_node.clone(),
        notes: core.notes.clone(),
        failure: core.failure.as_ref().map(core_to_wf_failure),
        usage: None,
        files_touched: Vec::new(),
        duration_ms: None,
    }
}

pub fn wf_to_core_failure(wf: &WfFailureDetail) -> CoreFailureDetail {
    CoreFailureDetail {
        message: wf.message.clone(),
        category: Some(wf.failure_class.to_string()),
        signature: wf.failure_signature.clone(),
    }
}

pub fn core_to_wf_failure(core: &CoreFailureDetail) -> WfFailureDetail {
    let failure_class = core
        .category
        .as_deref()
        .and_then(|c| c.parse::<FailureClass>().ok())
        .unwrap_or_else(|| classify_failure_reason(&core.message));
    WfFailureDetail {
        message: core.message.clone(),
        failure_class,
        failure_signature: core.signature.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_roundtrip_all_variants() {
        let wf_statuses = [
            WfStatus::Success,
            WfStatus::Fail,
            WfStatus::Skipped,
            WfStatus::PartialSuccess,
            WfStatus::Retry,
        ];
        for wf in &wf_statuses {
            let core = wf_to_core_status(wf);
            let back = core_to_wf_status(&core);
            assert_eq!(&back, wf, "roundtrip failed for {:?}", wf);
        }
    }

    #[test]
    fn outcome_roundtrip_success() {
        let wf = WfOutcome::success();
        let core = wf_to_core_outcome(&wf);
        let back = core_to_wf_outcome(&core);
        assert_eq!(back.status, WfStatus::Success);
        assert!(back.failure.is_none());
    }

    #[test]
    fn outcome_roundtrip_with_shared_fields() {
        let mut wf = WfOutcome::success();
        wf.preferred_label = Some("next".into());
        wf.suggested_next_ids = vec!["a".into(), "b".into()];
        wf.context_updates.insert("key".into(), json!("val"));
        wf.jump_to_node = Some("target".into());
        wf.notes = Some("hello".into());

        let core = wf_to_core_outcome(&wf);
        assert_eq!(core.preferred_label.as_deref(), Some("next"));
        assert_eq!(core.suggested_next_ids, vec!["a", "b"]);
        assert_eq!(core.context_updates.get("key"), Some(&json!("val")));
        assert_eq!(core.jump_to_node.as_deref(), Some("target"));
        assert_eq!(core.notes.as_deref(), Some("hello"));

        let back = core_to_wf_outcome(&core);
        assert_eq!(back.preferred_label, wf.preferred_label);
        assert_eq!(back.suggested_next_ids, wf.suggested_next_ids);
        assert_eq!(back.context_updates, wf.context_updates);
        assert_eq!(back.jump_to_node, wf.jump_to_node);
        assert_eq!(back.notes, wf.notes);
    }

    #[test]
    fn failure_roundtrip() {
        let wf_failure = WfFailureDetail {
            message: "api down".into(),
            failure_class: FailureClass::TransientInfra,
            failure_signature: Some("sig123".into()),
        };
        let core = wf_to_core_failure(&wf_failure);
        assert_eq!(core.message, "api down");
        assert_eq!(core.category.as_deref(), Some("transient_infra"));
        assert_eq!(core.signature.as_deref(), Some("sig123"));

        let back = core_to_wf_failure(&core);
        assert_eq!(back.message, "api down");
        assert_eq!(back.failure_class, FailureClass::TransientInfra);
        assert_eq!(back.failure_signature.as_deref(), Some("sig123"));
    }

    #[test]
    fn outcome_roundtrip_fail_with_failure() {
        let wf = WfOutcome::fail_classify("timeout talking to LLM");
        let core = wf_to_core_outcome(&wf);
        let back = core_to_wf_outcome(&core);
        assert_eq!(back.status, WfStatus::Fail);
        let f = back.failure.unwrap();
        assert_eq!(f.message, "timeout talking to LLM");
    }

    #[test]
    fn core_to_wf_failure_classifies_unknown_category() {
        let core = CoreFailureDetail {
            message: "something broke".into(),
            category: None,
            signature: None,
        };
        let wf = core_to_wf_failure(&core);
        // Should fall back to classify_failure_reason
        assert_eq!(wf.message, "something broke");
        // The class should be some valid FailureClass (exact value depends on classifier)
    }
}
