Write the feature requirements document at files-internal/product/features/ following the template structure:

## Overview

Clear summary of what the feature does and the value it delivers.

## Terminology

Key terms with definitions.

## Requirements

For each requirement:
- **REQ-NNN-XXX:** Named requirement
- **User Story:** As a [persona], I want to [action], so that I can [outcome]
- **Acceptance Criteria:** AC-NNN-XXX.N: When [condition], the system shall [behavior]

## Out of Scope

Adjacent capabilities, integrations, or behaviors explicitly excluded from this feature. Downstream agents must not implement anything listed here.

## Feature Behavior & Rules

Cross-requirement interactions, defaults, constraints, edge conditions.

Keep requirements implementation-agnostic. No data models, API shapes, or UI components. Focus only on observable behavior that a user or test can verify.
