Read the feature requirements doc matching $goal under files-internal/product/features/.
Read the foundation blueprints under files-internal/architecture/foundation-blueprints/ (backend.md, data-layer.md, frontend.md).
Read all existing feature blueprints under files-internal/architecture/features/.
Read the current codebase structure.

Write the feature blueprint at files-internal/architecture/features/ with:

## Solution Design

High-level technical architecture for this feature.

## Key Design Decisions

Rationale for approach choices, referencing foundation blueprint conventions.

## Data Model

Entities, fields, types, relationships. Reference existing tables from other blueprints. Define new tables where needed.

## API Implementation

Endpoints, HTTP methods, request/response models. Follow foundation backend conventions (FastAPI, SQLModel, dependency injection).

## UI Implementation

Key components, states, interactions. Use foundation frontend stack (React, shadcn/ui, Tailwind).

## Out of Scope

Adjacent concerns, optimizations, or features explicitly excluded from this blueprint. The implementing agent must not address anything listed here.

The blueprint must be detailed enough for an agent to implement without further clarification.

Before completing, self-verify:
- Every acceptance criterion from the feature requirements is addressed by at least one section
- Nothing in the blueprint contradicts the foundation blueprints
- The Out of Scope section explicitly excludes adjacent concerns that an implementing agent might drift into
