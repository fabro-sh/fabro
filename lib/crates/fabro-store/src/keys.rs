use fabro_types::{RunBlobId, RunId};

const RUNS_PREFIX: &str = "runs#";
const RUNS_INDEX_BY_START_PREFIX: &str = "runs#_index#by-start#";
const BLOBS_PREFIX: &str = "blobs#sha256#";

pub(crate) fn runs_index_by_start_prefix() -> &'static str {
    RUNS_INDEX_BY_START_PREFIX
}

pub(crate) fn runs_index_by_start_key(run_id: &RunId) -> String {
    format!(
        "{RUNS_INDEX_BY_START_PREFIX}{}#{run_id}",
        run_id.created_at().format("%Y-%m-%d")
    )
}

pub(crate) fn run_data_prefix(run_id: &RunId) -> String {
    format!("{RUNS_PREFIX}{run_id}#")
}

pub(crate) fn run_events_prefix(run_id: &RunId) -> String {
    format!("{}events#", run_data_prefix(run_id))
}

pub(crate) fn run_event_key(run_id: &RunId, seq: u32, epoch_ms: i64) -> String {
    format!("{}{seq:06}-{epoch_ms}", run_events_prefix(run_id))
}

pub(crate) fn blobs_prefix() -> &'static str {
    BLOBS_PREFIX
}

pub(crate) fn blob_key(id: &RunBlobId) -> String {
    format!("{BLOBS_PREFIX}{id}")
}

pub(crate) fn parse_event_seq(key: &str) -> Option<u32> {
    key.rsplit_once("#events#")?
        .1
        .split_once('-')?
        .0
        .parse()
        .ok()
}

pub(crate) fn parse_blob_id(key: &str) -> Option<RunBlobId> {
    key.strip_prefix(BLOBS_PREFIX)?.parse().ok()
}

pub(crate) fn parse_run_id_from_index_key(key: &str) -> Option<RunId> {
    let rest = key.strip_prefix(RUNS_INDEX_BY_START_PREFIX)?;
    let (_, run_id) = rest.split_once('#')?;
    run_id.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_types::RunId;

    #[test]
    fn top_level_keys_match_spec() {
        let run_id: RunId = "01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap();
        assert_eq!(
            run_event_key(&run_id, 7, 123),
            "runs#01JT56VE4Z5NZ814GZN2JZD65A#events#000007-123"
        );
        assert_eq!(
            runs_index_by_start_key(&run_id),
            format!(
                "runs#_index#by-start#{}#{run_id}",
                run_id.created_at().format("%Y-%m-%d")
            )
        );
    }

    #[test]
    fn sequence_keys_are_zero_padded() {
        let run_id: RunId = "01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap();
        assert_eq!(
            run_event_key(&run_id, 7, 123),
            "runs#01JT56VE4Z5NZ814GZN2JZD65A#events#000007-123"
        );
    }

    #[test]
    fn blob_keys_match_spec() {
        let blob_id = RunBlobId::new(b"summary");
        assert_eq!(blob_key(&blob_id), format!("blobs#sha256#{blob_id}"));
    }

    #[test]
    fn parse_helpers_extract_sequences_and_blob_ids() {
        assert_eq!(
            parse_event_seq("runs#01JT56VE4Z5NZ814GZN2JZD65A#events#000007-123"),
            Some(7)
        );
        let blob_id = RunBlobId::new(b"summary");
        assert_eq!(
            parse_blob_id(&format!("blobs#sha256#{blob_id}")),
            Some(blob_id)
        );
        assert_eq!(
            parse_run_id_from_index_key(
                "runs#_index#by-start#2026-03-27#01JT56VE4Z5NZ814GZN2JZD65A"
            ),
            Some("01JT56VE4Z5NZ814GZN2JZD65A".parse().unwrap())
        );
    }

    #[test]
    fn parse_helpers_reject_invalid_keys() {
        assert_eq!(parse_event_seq("runs#not-a-run#events#not-a-seq"), None);
        assert_eq!(parse_blob_id("blobs#not-a-uuid"), None);
        assert_eq!(
            parse_blob_id("blobs#01JT56VE4Z5NZ814GZN2JZD65A#not-a-blob"),
            None
        );
    }
}
