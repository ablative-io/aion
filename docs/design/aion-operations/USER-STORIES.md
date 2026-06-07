# Aion-Operations — User Stories

## Platform Operator — Deploying and operating Aion in production

**S1.** As a platform operator, I want to deploy aion-server from a TOML config file so that I can manage settings without rebuilding the binary.

**S2.** As a platform operator, I want Prometheus metrics on /metrics so that I can wire Aion into my existing monitoring stack and set alerts on workflow throughput and activity latency.

**S3.** As a platform operator, I want /health/live and /health/ready probes so that Kubernetes can manage pod lifecycle and route traffic only to healthy instances.

**S4.** As a platform operator, I want graceful shutdown on SIGTERM so that rolling deploys do not lose in-flight work or require manual intervention.

**S5.** As a platform operator, I want structured JSON logs with workflow_id and namespace fields so that I can correlate events across my log aggregation pipeline.

**S6.** As a platform operator, I want environment variable overrides so that I can configure Aion in container deployments where config files are awkward.

## Workflow Author — Writing workflows that use signals for coordination

**S7.** As a workflow author, I want signals sent to my workflow while it is suspended to be delivered when it resumes so that I do not lose coordination messages from external systems.

**S8.** As a workflow author, I want a clear error when I signal a terminal workflow so that I can distinguish between 'not yet delivered' and 'permanently unreachable'.

## Security Engineer — Enforcing tenant isolation and access control

**S9.** As a security engineer, I want namespace-scoped JWT authentication so that tenants cannot access each other's workflows or activity tasks.

**S10.** As a security engineer, I want requests without valid tokens to be rejected before reaching any handler so that unauthenticated traffic never touches business logic.

**S11.** As a security engineer, I want JWKS key rotation without server restart so that credential rotation does not cause downtime.

## Client SDK Developer — Building clients and workers that connect to Aion

**S12.** As a client SDK developer, I want a development server that starts with zero config so that I can run integration tests locally without provisioning infrastructure.
