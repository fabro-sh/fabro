Read all three layers and compare for alignment:

1. **Product docs:** files-internal/product/ (all feature requirements and acceptance criteria)
2. **Architecture docs:** files-internal/architecture/ (foundation blueprints, feature blueprints, system diagrams)
3. **Source code:** the actual implementation

For each feature, check:
- Do acceptance criteria in product/features/ match what the code actually does?
- Does the feature blueprint match the current code structure (data model, API, UI)?
- Do system diagrams reflect the current entities, flows, and components?
- Are foundation blueprints consistent with actual tech stack usage?

Report all mismatches with:
- Which layer is the source of truth (most recently intentionally changed)
- What needs to be updated
- The specific files and sections affected

Set context.drift_found to true if any drift detected, false otherwise.
