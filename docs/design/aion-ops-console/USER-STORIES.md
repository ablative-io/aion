# Aion-Dashboard — User Stories

## On-call Operator — Monitoring Running Workflows

**S1.** As an on-call operator, I want a filterable, paginated list of workflows by type, status, and time range so that I can quickly find what is running and what has failed without scrolling thousands of rows.

**S2.** As an on-call operator, I want the workflow list to update live so that status transitions and new workflows appear without me refreshing the page.

**S3.** As an on-call operator, I want a persistent connection indicator and automatic reconnection so that I always know whether what I am looking at is current, and the view resumes after a network blip.

**S4.** As an on-call operator, I want explicit empty and error states so that I can tell the difference between 'nothing matches' and 'the query failed', instead of staring at a spinner.

## Debugging Engineer — Investigating a Single Execution

**S5.** As a debugging engineer, I want a single workflow's full event history rendered as an ordered timeline so that I can read the story of what happened — activities, timers, signals, child workflows, and the outcome — at a glance.

**S6.** As a debugging engineer, I want an activity's scheduled/started/completed/failed events grouped and its retries visible so that I can see an activity's whole lifecycle as one unit rather than scattered rows.

**S7.** As a debugging engineer, I want to expand an event's payload so that I can inspect activity inputs/results, signal payloads, and error details when I need them without drowning in JSON by default.

**S8.** As a debugging engineer watching a running workflow, I want new events to stream into the timeline live and the terminal outcome to appear when it completes or fails so that I can watch the execution unfold.

**S9.** As a debugging engineer, I want to jump from a child-workflow entry to that child's own detail view so that I can trace a multi-workflow execution.

## Platform Operator — Operating a Multi-Tenant Server

**S10.** As a platform operator, I want to select a namespace so that the list, history, and live stream are all scoped to the tenant I am investigating.

**S11.** As a platform operator, I want a firehose feed of all events so that I can watch overall engine activity for observability.

## Frontend Maintainer — Maintaining the Dashboard Codebase

**S12.** As a frontend maintainer, I want the dashboard built on the same stack and conventions as apps/web so that I can work on it without learning a new toolchain.

**S13.** As a frontend maintainer, I want wire types generated from the Rust engine types so that the dashboard cannot silently drift out of sync with the server's API contract.

**S14.** As a frontend maintainer, I want the server contract isolated in one api module so that when the server's endpoint or protocol detail is finalised I change it in one place.
