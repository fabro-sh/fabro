use fabro_variable::{Error, VariableStore};

#[test]
fn load_missing_file_returns_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = VariableStore::load(dir.path().join("variables.json")).unwrap();

    assert!(store.list().is_empty());
}

#[test]
fn set_get_list_and_reload_variables() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("variables.json");
    let mut store = VariableStore::load(path.clone()).unwrap();

    let first = store.set("ZETA", "last", Some("Last variable")).unwrap();
    let second = store.set("ALPHA", "", None).unwrap();

    assert_eq!(first.name, "ZETA");
    assert_eq!(first.value, "last");
    assert_eq!(first.description.as_deref(), Some("Last variable"));
    assert_eq!(second.value, "");
    assert_eq!(store.get("ZETA").unwrap().value, "last");
    assert_eq!(
        store
            .list()
            .into_iter()
            .map(|variable| variable.name)
            .collect::<Vec<_>>(),
        vec!["ALPHA", "ZETA"]
    );

    let reloaded = VariableStore::load(path).unwrap();
    assert_eq!(reloaded.get("ALPHA").unwrap().value, "");
    assert_eq!(
        reloaded.get("ZETA").unwrap().description.as_deref(),
        Some("Last variable")
    );
}

#[test]
fn upsert_preserves_description_when_omitted_and_updates_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = VariableStore::load(dir.path().join("variables.json")).unwrap();

    let created = store
        .set("DEPLOY_ENV", "staging", Some("Deployment target"))
        .unwrap();
    let preserved = store.set("DEPLOY_ENV", "production", None).unwrap();
    let updated = store
        .set("DEPLOY_ENV", "preview", Some("Preview target"))
        .unwrap();

    assert_eq!(preserved.created_at, created.created_at);
    assert_eq!(preserved.description.as_deref(), Some("Deployment target"));
    assert_eq!(updated.description.as_deref(), Some("Preview target"));
    assert!(updated.updated_at >= preserved.updated_at);
}

#[test]
fn update_existing_preserves_description_and_reports_missing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = VariableStore::load(dir.path().join("variables.json")).unwrap();

    let created = store
        .set("DEPLOY_ENV", "staging", Some("Deployment target"))
        .unwrap();
    let updated = store
        .update_existing("DEPLOY_ENV", "production", None)
        .unwrap();

    assert_eq!(updated.created_at, created.created_at);
    assert_eq!(updated.value, "production");
    assert_eq!(updated.description.as_deref(), Some("Deployment target"));
    assert!(matches!(
        store.update_existing("MISSING", "value", None),
        Err(Error::NotFound(name)) if name == "MISSING"
    ));
}

#[test]
fn remove_deletes_variable_and_reports_missing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = VariableStore::load(dir.path().join("variables.json")).unwrap();
    store.set("DEPLOY_ENV", "staging", None).unwrap();

    store.remove("DEPLOY_ENV").unwrap();

    assert!(store.get("DEPLOY_ENV").is_none());
    assert!(matches!(
        store.remove("DEPLOY_ENV"),
        Err(Error::NotFound(name)) if name == "DEPLOY_ENV"
    ));
}

#[test]
fn env_style_names_are_required() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = VariableStore::load(dir.path().join("variables.json")).unwrap();

    for invalid in ["", "1BAD", "bad-name", "BAD.NAME"] {
        assert!(matches!(
            store.set(invalid, "value", None),
            Err(Error::InvalidName(name)) if name == invalid
        ));
    }

    store.set("_OK", "value", None).unwrap();
    store.set("OK_123", "value", None).unwrap();
}
