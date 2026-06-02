# Aion-Package — User Stories

## Engine — Loading and Registering Workflows

**S1.** As the engine, I want to load a .aion from a path or byte buffer and get a validated package so that I can register its modules without trusting loose files on disk.

**S2.** As the engine, I want the package to reject itself when its beams do not match its stored hash so that I never register a truncated or tampered workflow into the VM.

**S3.** As the engine, I want each deployed version's modules to have distinct, content-hash-namespaced names so that version N and version N+5 coexist and a months-long sleeping workflow never blocks a new deploy.

**S4.** As the engine, I want a loaded package to hand me the namespaced module names directly so that I apply one agreed naming scheme rather than deriving names myself.

## Workflow Author — Packaging and Deploying a Workflow

**S5.** As a workflow author, I want to package my compiled beams and a manifest into a single .aion file so that deploying my workflow is copying one file.

**S6.** As a workflow author, I want the package version to be derived from my compiled code so that I never hand-maintain a version string that could lie about what is in the package.

**S7.** As a workflow author, I want identical inputs to produce a byte-identical .aion so that my builds are reproducible and auditable.

## Operator — Inspecting and Trusting a Package

**S8.** As an operator, I want a .aion to be a plain ZIP with a readable manifest so that I can inspect what a package contains before deploying it.

**S9.** As an operator, I want the package format version checked on load so that a future format change is detected and reported, not silently misread.

## Toolchain — Optional Server-Side Authoring

**S10.** As the optional toolchain, I want to construct a .aion from compiled beams plus a manifest through a stable builder API so that the server-side compile-and-package loop has one place to produce packages.
