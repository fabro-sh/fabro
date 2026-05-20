use std::collections::HashMap;

use fabro_types::RunId;

pub(crate) const MANAGED_LABEL: &str = "sh.fabro.managed";
pub(crate) const RUN_ID_LABEL: &str = "sh.fabro.run_id";

#[cfg(any(feature = "docker", test))]
pub(crate) fn for_run(run_id: Option<&RunId>) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    insert_for_run(&mut labels, run_id);
    labels
}

#[cfg(any(feature = "daytona", test))]
pub(crate) fn merge_for_run(
    user_labels: Option<&HashMap<String, String>>,
    run_id: Option<&RunId>,
) -> HashMap<String, String> {
    let mut labels = user_labels.cloned().unwrap_or_default();
    insert_for_run(&mut labels, run_id);
    labels
}

fn insert_for_run(labels: &mut HashMap<String, String>, run_id: Option<&RunId>) {
    labels.insert(MANAGED_LABEL.to_string(), "true".to_string());
    if let Some(run_id) = run_id {
        labels.insert(RUN_ID_LABEL.to_string(), run_id.to_string());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_types::RunId;

    use super::*;

    fn conservative_daytona_key(key: &str) -> bool {
        key.chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_'))
    }

    #[test]
    fn managed_label_keys_match_docker_and_use_conservative_ascii() {
        assert_eq!(MANAGED_LABEL, "sh.fabro.managed");
        assert_eq!(RUN_ID_LABEL, "sh.fabro.run_id");
        assert!(conservative_daytona_key(MANAGED_LABEL));
        assert!(conservative_daytona_key(RUN_ID_LABEL));
    }

    #[test]
    fn managed_labels_include_run_id_when_present() {
        let run_id: RunId = "01HY0000000000000000000000".parse().unwrap();
        let labels = for_run(Some(&run_id));

        assert_eq!(labels.get(MANAGED_LABEL).map(String::as_str), Some("true"));
        assert_eq!(
            labels.get(RUN_ID_LABEL).map(String::as_str),
            Some("01HY0000000000000000000000")
        );
    }

    #[test]
    fn managed_labels_override_reserved_user_labels() {
        let run_id: RunId = "01HY0000000000000000000000".parse().unwrap();
        let user_labels = HashMap::from([
            ("team".to_string(), "platform".to_string()),
            (MANAGED_LABEL.to_string(), "false".to_string()),
            (RUN_ID_LABEL.to_string(), "wrong".to_string()),
        ]);

        let labels = merge_for_run(Some(&user_labels), Some(&run_id));

        assert_eq!(labels.get("team").map(String::as_str), Some("platform"));
        assert_eq!(labels.get(MANAGED_LABEL).map(String::as_str), Some("true"));
        assert_eq!(
            labels.get(RUN_ID_LABEL).map(String::as_str),
            Some("01HY0000000000000000000000")
        );
    }
}
