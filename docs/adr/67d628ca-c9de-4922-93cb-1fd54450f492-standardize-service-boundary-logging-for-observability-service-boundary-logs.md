# Standardize Service Boundary Logging for Observability: Service Boundary Logs

Status: proposed
Date: 2024-01-15
Deciders: Detection Pipeline (automated)

## Activation

This ADR is ACTIVE for all service implementations and applies to all components that define service boundaries, including API handlers, server implementations, and client query interfaces.

## Context

- The codebase exhibits a consistent pattern of logging at service boundaries across multiple components (fabro-server and fabro-web), indicating an architectural decision to instrument entry and exit points of service operations
- Service boundary logging is critical for distributed system observability, enabling request tracing, performance monitoring, and debugging across service interactions
- The pattern appears in both backend Rust server implementations and frontend TypeScript query layers, suggesting a cross-stack architectural concern
- With 3 files showing this pattern at 90.17% confidence, this represents a deliberate architectural choice rather than ad-hoc logging practices
- The facet 'boundaries.service_definitions' indicates this pattern specifically targets the interfaces where services are defined and exposed

## Problem Statement

Without standardized logging at service boundaries, distributed systems suffer from poor observability, making it difficult to trace requests across services, diagnose performance bottlenecks, identify failure points, and understand system behavior in production. Ad-hoc logging approaches lead to inconsistent log formats, missing context, and gaps in observability coverage.

## Decision

1. MUST: Service boundary logs MUST include correlation identifiers (request IDs, trace IDs) to enable distributed tracing across service calls

## Policy Block

- MUST Service boundary logs MUST include correlation identifiers (request IDs, trace IDs) to enable distributed tracing across service calls

In scope:
- HTTP API handlers and endpoints
- RPC service method implementations
- GraphQL resolvers and query handlers
- Message queue consumers and producers
- Database query interfaces that represent service boundaries
- External service client wrappers
- WebSocket connection handlers

Out of scope:
- Internal utility functions that do not represent service boundaries
- Private helper methods within a service implementation
- Data transformation and validation logic
- Pure business logic functions without external interactions
- Unit test code and test fixtures

Exceptions:
- EXC-001: High-frequency, low-value operations (health checks, metrics endpoints) where logging every request would create excessive log volume
- EXC-002: Performance-critical hot paths where logging overhead is measured and documented to exceed acceptable latency budgets

## Rationale

- The detection of this pattern across 3 files with 90.17% confidence indicates this is an established architectural practice in the codebase, representing proven value in production systems
- Service boundary logging provides the minimal necessary observability coverage for distributed systems without requiring invasive instrumentation throughout the codebase
- Standardizing logging at boundaries creates consistent observability across heterogeneous technology stacks (Rust backend, TypeScript frontend), enabling unified monitoring and debugging workflows
- This approach balances observability needs with performance concerns by focusing logging on key interaction points rather than instrumenting every function

## Consequences

Positive:
- Improved ability to trace requests through distributed system components, reducing mean time to resolution (MTTR) for production issues
- Consistent log structure across services enables automated log aggregation, analysis, and alerting
- Clear observability boundaries make it easier for developers to understand where logging is required versus optional
- Correlation identifiers at service boundaries enable powerful distributed tracing capabilities without complex instrumentation

Negative:
- Additional logging overhead at service boundaries may impact latency for high-frequency operations, requiring careful performance monitoring
- Developers must remember to implement boundary logging for all new service endpoints, creating potential for human error
- Log volume may increase significantly in high-traffic systems, requiring investment in log management infrastructure
- Maintaining consistent logging patterns across multiple technology stacks (Rust, TypeScript) requires ongoing coordination and code review discipline

## Alternatives

- Comprehensive instrumentation logging throughout the entire codebase, not just at service boundaries (rejected)
  Rejected because: Creates excessive log volume, performance overhead, and maintenance burden. Most internal function calls do not provide actionable observability value compared to boundary logging.
  When valid: May be appropriate for specific critical subsystems during active debugging or performance optimization efforts, but not as a general architectural pattern
- Rely solely on distributed tracing frameworks (OpenTelemetry, Jaeger) without explicit boundary logging (rejected)
  Rejected because: Tracing frameworks add complexity and dependencies, may not be available in all environments, and logs provide complementary information (detailed context, error messages) that traces don't capture well
  When valid: Could be adopted in addition to boundary logging as systems mature, but should not replace foundational logging practices
- Implement logging through middleware/interceptors only, without explicit logging in service code (deferred)
  Rejected because: While middleware can handle many boundary logging concerns, some service-specific context and business logic details are best logged within the service implementation itself
  When valid: Middleware-based logging should be used where possible for consistency, with explicit service logging for context that middleware cannot capture

## Risks

- Inconsistent implementation across teams and services leads to gaps in observability coverage
  Mitigation: Implement automated linting rules to detect missing boundary logging, provide code templates and examples, include boundary logging checks in code review checklists
  Owner: Engineering team leads and platform team
- Sensitive data leakage through logs if developers are not careful about what context they include
  Mitigation: Implement automated PII detection in logs, provide sanitization utilities, conduct security training on logging best practices, enable log redaction in production
  Owner: Security team and engineering team
- Performance degradation in high-throughput services due to synchronous logging overhead
  Mitigation: Use async logging frameworks, implement log sampling for high-frequency endpoints, monitor logging performance impact, establish performance budgets for logging overhead
  Owner: Platform team and service owners

## Implementation Notes

- Create shared logging utilities or macros that encapsulate boundary logging patterns, making it easy for developers to add consistent logging with minimal boilerplate
- For Rust services, leverage structured logging crates like 'tracing' or 'slog' that provide zero-cost abstractions and async logging capabilities
- For TypeScript/JavaScript services, use structured logging libraries like 'pino' or 'winston' with JSON formatters for consistency with backend logs
- Establish a standard set of log fields for service boundaries: timestamp, service_name, operation, request_id, duration_ms, status, error_message (if applicable)
- Configure log aggregation systems (ELK, Splunk, CloudWatch) to parse and index boundary logs for efficient querying and alerting
- Document logging patterns in service templates and starter kits to ensure new services follow established practices from the beginning

## Continuation Context


Verify commands:
- grep -r 'log.*request' --include='*.rs' --include='*.ts' lib/crates/fabro-server/src/server/ apps/fabro-web/app/lib/ | wc -l
- rg '(info!|warn!|error!).*\(|console\.(log|info|warn|error)' --type rust --type ts -g '*/handler/*' -g '*/queries.*'
- find . -name 'server.rs' -o -name 'handler*.rs' -o -name 'queries.ts' | xargs grep -l 'trace_id\|request_id\|correlation'

Accept when:
- All service handler files (server.rs, handler/*.rs, queries.ts) contain logging statements at operation entry and exit points
- Grep commands identify logging patterns in at least 80% of service boundary files
- Code review confirms presence of correlation identifiers (request_id, trace_id) in boundary logs

## Enforcement

- Verified by: Automated code review checks using static analysis tools (clippy for Rust, ESLint for TypeScript) with custom rules for boundary logging
- Verified by: Manual code review checklist items requiring reviewers to verify boundary logging presence
- Verified by: CI pipeline integration tests that verify log output from service endpoints contains required fields
- Verified by: Periodic observability audits reviewing log coverage across services
- Violation handling: CI pipeline warnings for missing boundary logging patterns (non-blocking initially, blocking after grace period)
- Violation handling: Code review feedback requiring addition of boundary logging before merge approval
- Violation handling: Quarterly observability reports identifying services with insufficient logging coverage
- Violation handling: Escalation to architecture review for repeated violations or services with poor observability
- Exception process: Submit exception request to team lead or architect with documented rationale (performance impact, alternative observability approach)
- Exception process: For performance-critical paths, provide benchmark data demonstrating logging overhead exceeds acceptable latency budget
- Exception process: Document approved exceptions in service README and architecture decision log
- Exception process: Review exceptions quarterly to determine if mitigations (async logging, sampling) can eliminate the need for exception