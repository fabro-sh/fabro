# Adopt Distributed Tracing for Public API Observability: Implement Sampling Strategies

Status: proposed
Date: 2024-01-15
Deciders: Detection Pipeline (automated)

## Activation

This ADR is ACTIVE for all public/external API implementations and integration patterns. All new public API endpoints and external integrations MUST implement distributed tracing as specified herein.

## Context

- Public and external APIs require comprehensive observability to diagnose issues across distributed system boundaries where direct debugging is not feasible
- The pattern was detected with 90.27% confidence across 3 critical infrastructure files (terminal.rs, lib.rs, stall.rs) in the fabro crate ecosystem, indicating a systematic approach to tracing
- The facet 'obs.tracing' suggests this pattern specifically addresses observability through distributed tracing instrumentation
- External API consumers and integration partners require visibility into request flows, latency bottlenecks, and error propagation paths
- Without standardized tracing, debugging cross-service issues becomes prohibitively expensive and time-consuming

## Problem Statement

Public and external APIs lack consistent observability instrumentation, making it difficult to diagnose performance issues, trace request flows across service boundaries, and provide actionable debugging information to external consumers. This creates operational blind spots and increases mean time to resolution (MTTR) for production incidents.

## Decision

1. SHOULD: APIs SHOULD implement sampling strategies to balance observability needs with performance overhead (e.g., 100% for errors, 10% for successful requests)

## Policy Block

- SHOULD APIs SHOULD implement sampling strategies to balance observability needs with performance overhead (e.g., 100% for errors, 10% for successful requests)

In scope:
- All HTTP/REST API endpoints exposed to external consumers
- GraphQL APIs and gRPC services with external clients
- Webhook handlers and callback endpoints
- Public SDK methods that initiate cross-service operations
- Integration adapters and third-party service connectors

Out of scope:
- Internal library functions that do not cross service boundaries
- Pure computation functions without I/O operations
- Development and test environments (may use mock tracers)
- CLI tools and batch jobs (unless they interact with public APIs)

Exceptions:
- EXC-001: Performance-critical hot paths where tracing overhead exceeds 5% of request latency
- EXC-002: Legacy APIs scheduled for deprecation within 6 months

## Rationale

- The pattern detection across 3 files with 90.27% confidence indicates this is an established architectural practice in the codebase, not an isolated implementation
- Distributed tracing provides end-to-end visibility that is essential for debugging issues in microservices architectures where requests span multiple services
- Standardizing on obs.tracing facet ensures consistent instrumentation patterns across the organization, reducing cognitive load and improving debugging efficiency
- External API consumers benefit from trace IDs in error responses, enabling them to provide actionable information when reporting issues

## Consequences

Positive:
- Reduced mean time to resolution (MTTR) for production incidents through comprehensive request flow visibility
- Improved developer experience with standardized tracing patterns across all public APIs
- Enhanced external consumer support with traceable request identifiers for issue reporting
- Better capacity planning and performance optimization through latency distribution analysis

Negative:
- Increased operational complexity with additional infrastructure for trace collection and storage
- Minor performance overhead (typically 1-3% latency increase) from tracing instrumentation
- Development time investment required to instrument existing APIs and train teams on tracing best practices
- Potential for trace data explosion requiring careful sampling strategy and retention policies

## Alternatives

- Structured logging only without distributed tracing (rejected)
  Rejected because: Logs lack the causal relationships and timing information needed to reconstruct request flows across service boundaries. Correlation requires manual log aggregation which is error-prone and time-consuming.
  When valid: May be sufficient for monolithic applications with single-service request handling
- Metrics-based observability with counters and histograms (rejected)
  Rejected because: Metrics provide aggregate statistics but cannot trace individual request paths or identify specific failure scenarios. Complementary to tracing but insufficient as sole observability strategy.
  When valid: Appropriate for high-level health monitoring and alerting, should be used alongside tracing
- Vendor-specific tracing solutions (e.g., AWS X-Ray, Datadog APM) (deferred)
  Rejected because: Not rejected but deferred to implementation phase. Vendor choice should align with existing infrastructure while maintaining standard trace context propagation.
  When valid: Valid if vendor solution supports OpenTelemetry or W3C Trace Context standards for interoperability

## Risks

- Trace data volume may exceed storage capacity or budget constraints, leading to incomplete observability
  Mitigation: Implement adaptive sampling strategies with 100% sampling for errors and configurable rates for successful requests. Establish retention policies (e.g., 7 days for all traces, 30 days for error traces).
  Owner: Platform Engineering Team
- Inconsistent tracing implementation across teams may result in fragmented observability
  Mitigation: Provide shared tracing libraries and middleware with sensible defaults. Conduct code reviews specifically checking for tracing compliance. Create runbooks and training materials.
  Owner: Engineering Team
- Performance overhead from tracing may impact latency-sensitive APIs
  Mitigation: Benchmark tracing overhead during implementation. Use asynchronous trace export to minimize request latency impact. Allow exceptions for proven performance-critical paths.
  Owner: API Development Team

## Implementation Notes

- Use OpenTelemetry SDK for language-agnostic tracing instrumentation with broad ecosystem support
- Implement tracing middleware at the API gateway/framework level to automatically instrument all endpoints with minimal code changes
- Create shared libraries or decorators that encapsulate tracing logic for common patterns (database access, HTTP clients, message queues)
- Include trace_id in API error responses (e.g., in X-Trace-Id header) to enable external consumers to reference specific requests when reporting issues

## Continuation Context


Verify commands:
- grep -r 'tracing::instrument\|#\[instrument\]\|tracer\.start_span' lib/crates/*/src/ | wc -l
- grep -r 'obs\.tracing\|opentelemetry\|trace_context' lib/crates/fabro-*/src/ --include='*.rs'
- cargo test --package fabro-test -- tracing --nocapture 2>&1 | grep -i 'span\|trace'

Accept when:
- All public API endpoints in fabro-sandbox, fabro-test, and fabro-core crates demonstrate tracing instrumentation with entry/exit spans
- Verification commands show tracing patterns present in at least 80% of public API implementation files
- Integration tests successfully propagate trace context across service boundaries and validate span hierarchy

## Enforcement

- Verified by: Automated CI pipeline checks for tracing instrumentation in new API endpoints using static analysis
- Verified by: Code review checklist includes verification of distributed tracing implementation
- Verified by: Integration test suite validates trace context propagation and span attribute completeness
- Violation handling: CI pipeline fails if new public API endpoints lack tracing instrumentation
- Violation handling: Code review blocks merge if tracing requirements are not met without documented exception
- Violation handling: Quarterly audits identify non-compliant APIs with remediation plans required within 30 days
- Exception process: Submit exception request to architecture review board with performance benchmarks or deprecation timeline
- Exception process: Document exception in API specification and architectural decision log
- Exception process: Re-evaluate exceptions quarterly to determine if circumstances have changed