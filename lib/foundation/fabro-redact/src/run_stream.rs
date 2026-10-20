use fabro_types::{RunStreamItem, RunStreamItemKind};

use crate::{Region, entropy, gitleaks};

/// Redact a run stream item at a public output boundary.
///
/// Preserve the envelope's run ID in Fabro's generated URL and branch.
/// Only that exact token is exempt from entropy detection; known credential
/// patterns and all other event content still receive normal scanning.
pub fn redact_run_stream_item(mut item: RunStreamItem) -> RunStreamItem {
    let reference = generated_reference(&item);
    item.item = crate::redact_json_value(item.item);
    if let Some((field, redacted)) = reference {
        item.item["record"][field] = redacted.into();
    }
    item
}

fn generated_reference(item: &RunStreamItem) -> Option<(&'static str, String)> {
    if item.kind != RunStreamItemKind::Platform {
        return None;
    }
    let run_id = item.run_id.to_string();
    let record = item.item.get("record")?;
    let field = match record.get("kind")?.as_str()? {
        "run.created" => "web_url",
        "run.branch" => "run_branch",
        _ => return None,
    };
    let text = record.get(field)?.as_str()?;
    let generated = match field {
        "web_url" => text.ends_with(&format!("/runs/{run_id}")),
        _ => text == format!("fabro/run/{run_id}"),
    };
    if !generated {
        return None;
    }
    let id_region = Region {
        start: text.len() - run_id.len(),
        end:   text.len(),
    };
    let mut regions = entropy::find_entropy_regions(text);
    regions.retain(|region| *region != id_region);
    regions.extend(gitleaks::find_gitleaks_regions(text));
    Some((field, crate::redact_regions(text, regions)))
}

#[cfg(test)]
mod tests {
    use fabro_types::RunStreamItemKind;
    use serde_json::{Value, json};

    use super::*;

    const RUN_ID: &str = "01M5228EHCT0P73Z4BYGDRWJNS";
    const OTHER_ID: &str = "01M5228EHCT0P73Z4BYGDRWJNV";
    const SECRET: &str = "xK9mZ2vL8nQ5rT1wY4bC7dF0gH3jE6p";

    fn platform(record: &Value) -> RunStreamItem {
        RunStreamItem {
            run_id:      RUN_ID.parse().unwrap(),
            stream_seq:  1,
            kind:        RunStreamItemKind::Platform,
            id:          "1".to_string(),
            recorded_at: 0,
            item:        json!({"seq": 1, "record": record}),
        }
    }

    #[test]
    fn generated_run_references_survive_high_entropy_ids() {
        // Both IDs cross the scanner's entropy threshold deterministically.
        assert_eq!(crate::redact_string(RUN_ID), "REDACTED");
        assert_eq!(crate::redact_string(OTHER_ID), "REDACTED");
        for (kind, field, text) in [
            (
                "run.created",
                "web_url",
                format!("http://localhost:3000/runs/{RUN_ID}"),
            ),
            ("run.branch", "run_branch", format!("fabro/run/{RUN_ID}")),
        ] {
            let input = platform(&json!({"kind": kind, field: text, "content": RUN_ID}));
            let redacted = redact_run_stream_item(input.clone());
            assert_eq!(redacted.run_id, input.run_id);
            assert_eq!(redacted.item["record"][field], text);
            assert_eq!(redacted.item["record"]["content"], "REDACTED");
            assert_eq!(redact_run_stream_item(redacted.clone()), redacted);
        }
    }

    #[test]
    fn generated_url_still_redacts_secrets_outside_the_run_id() {
        for secret in [SECRET, "AKIAYRWQG5EJLPZLBYNP", RUN_ID] {
            let url = format!("https://{secret}@example.com/runs/{RUN_ID}");
            let redacted = redact_run_stream_item(platform(&json!({
                "kind": "run.created", "web_url": url,
            })));
            assert_eq!(
                redacted.item["record"]["web_url"],
                format!("https://REDACTED@example.com/runs/{RUN_ID}")
            );
        }
    }

    #[test]
    fn unrelated_values_and_records_keep_generic_redaction() {
        for record in [
            json!({"kind": "run.created", "web_url": format!("http://localhost/runs/{OTHER_ID}")}),
            json!({"kind": "run.created", "web_url": format!("http://localhost/{RUN_ID}")}),
            json!({"kind": "run.created", "web_url": format!("http://localhost/runs/{RUN_ID}?token={SECRET}")}),
            json!({"kind": "run.branch", "run_branch": format!("fabro/run/{OTHER_ID}")}),
            json!({"kind": "run.branch", "run_branch": format!("feature/{RUN_ID}")}),
            json!({"kind": "run.notice", "web_url": format!("http://localhost/runs/{RUN_ID}")}),
            json!({"kind": "run.created", "web_url": null}),
        ] {
            let input = platform(&record);
            let expected = crate::redact_json_value(input.item.clone());
            assert_eq!(redact_run_stream_item(input).item, expected);
        }
        let mut petri = platform(&json!({
            "kind": "run.created", "web_url": format!("http://localhost/runs/{RUN_ID}"),
        }));
        petri.kind = RunStreamItemKind::Petri;
        let expected = crate::redact_json_value(petri.item.clone());
        assert_eq!(redact_run_stream_item(petri).item, expected);
    }
}
