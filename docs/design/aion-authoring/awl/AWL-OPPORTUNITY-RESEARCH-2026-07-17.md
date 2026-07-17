<!-- STATUS: RESEARCH RECORD (2026-07-17). Evidence-backed opportunity map produced by a delegated
research pass (norn/gpt-5.6-sol xhigh, external-landscape preset, web primary sources; session 8805bb01,
envelope ~/.norn/delegations/claude-research-awl-r4.json). Feeds AWL-BIG-PICTURE.md §6; the ranked
shortlist at the end is the seed for the next design conversation. Every claim cites its source. -->

# AWL opportunity map — owning the language, compiler, VM, and durable runtime

## Evidence posture

The current repository supports a strong but bounded starting point. AWL workflow files already have a closed deterministic vocabulary, typed action requirements, source-spanned checking, generated schemas, and direct MIR→BEAM compilation; the runtime has recorded-history replay and content-hash package versions. Worker **interfaces** exist, but AWL-authored worker bodies are a direction capture, not a ratified implementation. Server-owned worker placement/supervision and the OS-process/cgroup driver are also designs, not shipped facts. The beamr AOT path is explicitly a long-range north-star with prerequisite correctness and opcode-coverage work. Sources: `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-2-SPEC.md:33-47,233-256,471-504`; `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-BIG-PICTURE.md:41-72`; `/Users/tom/Developer/ablative/aion/docs/design/WORKER-AUTHORING-STORY.md:1-8,75-125`; `/Users/tom/Developer/ablative/aion/docs/design/WORKER-DEPLOYMENT.md:47-62,221-290`; `/Users/tom/Developer/ablative/beamr/docs/AOT-NORTH-STAR.md:1-6,78-90,123-139`.

---

# 1. CROSS-BOUNDARY TYPE CHECKING

## The prior-art map

### Ballerina — network interactions belong in the language

**What it got right.** Ballerina makes client objects, service objects, remote methods, resources, and listener-mediated dispatch first-class language concepts. Calls through those constructs are statically checked against object and function types, rather than being hidden behind untyped HTTP strings. That is the closest mainstream language precedent for saying “the compiler understands that this expression crosses a network boundary.” Source: https://ballerina.io/spec/lang/master/.

**Where the guarantee stops.** The Ballerina specification checks Ballerina source and the contracts represented in its object types. It does not define a deployment gate proving that an independently built, arbitrary non-Ballerina server actually implements the client-side contract. Protocol adapters/listeners and real remote deployment remain outside the closed compilation unit. Source: https://ballerina.io/spec/lang/master/.

**What AWL can uniquely add.** AWL can make the action declaration simultaneously be the workflow-side function type, the AWL-worker implementation obligation, the generated external-SDK contract, the wire schema, and the registration/deploy admission rule. Ballerina owns the source-language network expression; Aion also owns the task queue, worker registry, content-hash deployment identity, and dispatch path. Existing seams are named in `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-2-SPEC.md:233-256` and `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-BIG-PICTURE.md:74-92`.

### Choral and HasChor — write the global program, project the endpoints

**What they got right.** Choreographic programming starts from one global description of the distributed interaction and generates local endpoint programs by endpoint projection. HasChor explicitly describes a choreography as one program for the whole distributed system and implements endpoint projection as a Haskell library; Choral exposes endpoint projection as a compiler operation. This removes a major class of separately authored client/server disagreement because the endpoints are derived from one source. Sources: https://arxiv.org/abs/2303.00924; https://github.com/choral-lang/choral.

**Where the guarantee stops.** The strength comes from a closed world: endpoints must be generated or represented in the choreography’s supported model. The primary sources inspected do not establish durable retry semantics, side-effect idempotence, content-addressed deployment compatibility, or conformance of independently implemented polyglot endpoints. Choral’s public README is also compiler-centric and sparse on production deployment guarantees. Sources: https://arxiv.org/abs/2303.00924; https://github.com/choral-lang/choral.

**What AWL can uniquely add.** For AWL-authored workflows and workers, Aion can take the same “one global source” advantage without necessarily generating the two sides from one choreography: the compiler can check the workflow call graph and every worker action body against one canonical action contract, then package both under immutable hashes. For Python/Gleam/Rust/ACP workers, the system can degrade from compile-time proof to generated stubs plus deploy/registration conformance. This creates a useful guarantee ladder rather than pretending polyglot code has the same proof strength as closed-world AWL.

### Session types and Scribble-style global protocols — check the conversation, not just each message

**What they got right.** Multiparty session-type systems and Scribble model a global protocol, project local endpoint obligations, and check that communication follows the allowed sequence and branch structure. Their central lesson is that matching request/response record schemas is weaker than checking the legal conversation: who sends next, which alternatives are legal, and whether all participants agree. Primary project source: https://github.com/scribble/scribble-java.

**Where the guarantee stops.** Practical guarantees depend on every participant using generated endpoint APIs, static types, or runtime monitors and on the transport satisfying the model’s assumptions. Session types generally do not establish whether a side-effecting operation is safe to retry, whether a deployment contains the intended implementation, or whether durable replay/version migration is valid.

**What AWL can uniquely add.** AWL’s workflow graph already contains a protocol: action dispatches, awaits, signals, children, outcomes, retries, and routes. Aion can check not only the payload type but also protocol facts that normal RPC IDLs cannot see: an action exists on the selected worker contract; its result reaches only compatible paths; retry posture is legal; every durable outcome is handled; and the deployed worker version is compatible with the workflow package version. Sources: `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-2-SPEC.md:279-292,394-439,471-504`; `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-FLOW-VOCABULARY.md:55-81`.

### Unison abilities and content-addressed remote computation — effect identity and code identity are first-class

**What it got right.** Unison’s important precedent is the combination of content-addressed definitions and explicit abilities/effects: code identity is derived from its definition and dependencies rather than a mutable textual module name, and effects can be represented in function types. That makes code mobility, caching, and distributed execution much less dependent on matching mutable deployment names. Primary overview: https://www.unison-lang.org/learn/the-big-idea/; project source: https://github.com/unisonweb/unison.

**Where the guarantee stops.** This is not evidence that Unison statically proves an independently implemented, polyglot worker behind an arbitrary queue conforms to the caller’s contract. Its strongest properties apply within the Unison code and runtime model.

**What AWL can uniquely add.** Aion already has content-hash package versions and durable version routing. AWL can attach the action contract, capability set, and compatibility metadata to the same immutable identity, so “which implementation did this durable call mean?” becomes an answerable, replay-stable question. Existing content-hash/version evidence: `/Users/tom/Developer/ablative/aion/CLAUDE.md:35`; `/Users/tom/Developer/ablative/aion/crates/aion/tests/engine_reload.rs:327-390`; `/Users/tom/Developer/ablative/aion/crates/aion/tests/deploy_persistence_e2e.rs:466-526`.

### Temporal, Restate, and Inngest typed SDKs — excellent local typing, incomplete system closure

**Temporal.** In TypeScript, `proxyActivities<typeof activities>()` can infer activity argument and result types, and worker creation registers activity implementations and a task-queue name. This is strong in-repository typing, but the runtime worker fleet remains separately deployed and selected through registration/task-queue configuration; the SDK type system does not prove before deployment that every worker reachable through that queue implements the expected activity set and wire contract. Sources: https://docs.temporal.io/develop/typescript/core-application; https://typescript.temporal.io/api/namespaces/workflow/#proxyActivities; https://typescript.temporal.io/api/interfaces/worker.WorkerOptions.

**Restate.** Restate service definitions and generated/type-safe clients align handlers and calls within supported SDKs, and its service-communication APIs preserve useful request/response typing. The boundary still crosses separately deployed service endpoints and polyglot SDKs; runtime discovery/deployment is not a single whole-program compiler proof. Sources: https://docs.restate.dev/develop/ts/services; https://docs.restate.dev/develop/ts/service-communication; https://github.com/restatedev/sdk-typescript.

**Inngest.** TypeScript event schemas and imported function references can make events and `step.invoke` calls type-aware. Function IDs, events, and separately deployed handlers remain runtime identities, so the guarantee does not become a universal pre-deploy proof of every implementation that could receive work. Sources: https://www.inngest.com/docs/typescript; https://www.inngest.com/docs/reference/functions/step-invoke.

**What they got right.** They prove that typed SDK ergonomics matter: callers should see normal language-level signatures, not manually synchronized JSON documents.

**Where they stop.** Their type guarantees are primarily SDK/language-local. They do not own one compiler that checks both orchestration and worker bodies, nor do they generally reject a worker registration because its complete action surface is incompatible with a particular workflow package.

**What AWL can uniquely add.** The Aion server already owns the registration door. AWL can make registration an attestation against a content-addressed contract, not merely “a worker says it serves queue X.” That is the missing half explicitly identified in `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-BIG-PICTURE.md:74-92`.

### Smithy, protobuf, and Buf — the production benchmark for contract artifacts

**What they got right.** Smithy and protobuf make the interface a persistable IDL that generates clients, servers, and codecs. Smithy has model validators; protobuf has durable field-number/wire rules; Buf adds machine-enforced breaking-change checks. This is the strongest production precedent for one boundary contract crossing languages and surviving build pipelines. Sources: https://smithy.io/2.0/spec/model-validation.html; https://smithy.io/2.0/guides/building-codegen/index.html; https://protobuf.dev/programming-guides/proto3/; https://buf.build/docs/breaking/.

**Where the guarantee stops.** IDL systems check message and service shape, not the worker implementation’s business behavior, command retry safety, workflow topology, durable replay legality, or whether a worker with a matching transport schema actually has the required operational semantics.

**What AWL can uniquely add.** Steal the artifact discipline—not the second authored language. AWL’s declared type/action surface can emit a stable contract descriptor with compatibility metadata, generated SDK stubs, codecs, and registration proofs. That descriptor can be checked together with workflow topology, effect posture, and package version. The existing competitive research already recommends stealing protobuf/Buf’s stable-identity and breaking-gate lessons rather than making IDL the primary authoring surface: `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/COMPETITIVE-RESEARCH-2026-07-02.md:79-87,118-130`.

### Dhall and CUE — whole-configuration checking, not whole-system implementation checking

**What they got right.** Dhall supplies typed, total, normalizing configuration; CUE unifies data and constraints so configuration can be validated and composed before use. They demonstrate that a “configuration language” need not be stringly or limited to JSON-schema shape checking. Sources: https://docs.dhall-lang.org/; https://cuelang.org/docs/concept/the-logic-of-cue/.

**Where the guarantee stops.** They validate the declared configuration/value model. They do not compile worker implementations, supervise commands, or prove that a runtime endpoint performs the action represented by the configuration.

**What AWL can uniquely add.** AWL can retain their small-language discipline while checking executable semantics because Aion owns the executor. A retry word, command invocation, action type, capability, and worker registration do not need to be opaque host-program conventions.

## Who has come closest?

There are three different winners, depending on the meaning of “everything”:

1. **Closest to whole-system static checking in research:** Choral/HasChor and multiparty session types, because one global program/protocol is projected to endpoints. Their weakness is practical closure around arbitrary workers, durability, side effects, and deployment.
2. **Closest production cross-language contract discipline:** protobuf/Smithy plus a Buf-style compatibility gate. Their weakness is that they stop at interface/wire shape.
3. **Closest integrated network-aware application language:** Ballerina. Its compiler understands the network operation, but does not prove an arbitrary separately deployed implementation satisfies it.

No cited system combines all of these in one product boundary: a small checked orchestration language, worker implementations in the same language, generated polyglot worker contracts, durable execution/replay, content-addressed deploys, and a registration path that can reject incompatible workers. That combination is the AWL opportunity.

## What “check absolutely everything from the get-go” would actually require

This cannot literally prove arbitrary external behavior. The defensible target is: **if a value or operation crosses an Aion-owned boundary, it either has a checked proof artifact or is explicitly marked as a dynamic escape hatch.** Reaching that target requires:

- one canonical action identity and versioned contract descriptor, derived from AWL rather than re-authored;
- workflow call-site checking for names, named parameters, result types, routes, retry legality, and capabilities;
- AWL-worker body checking against the same action declaration;
- generated Python/Gleam/Rust/TypeScript stubs and codecs for external workers;
- registration/deploy admission that compares the worker-provided descriptor with every served workflow requirement and refuses incompatible/missing actions;
- stable wire identities plus a breaking-change gate for durable history and in-flight calls;
- version binding so a workflow package records which compatible worker contract/version it targeted;
- effect metadata covering retry/idempotency posture, timeout, cancellation, and required capabilities—not merely JSON shape;
- codec round-trip/golden tests across languages;
- no untyped “call by arbitrary string” in checked AWL, except an explicit dynamic escape hatch whose result is validated before re-entering typed flow.

The cheap half is AWL↔AWL because one compiler owns both sides. The expensive half is truthful polyglot conformance: generated artifacts, build integration, registration attestations, compatibility rules, and honest treatment of semantics that schemas cannot prove.

---

# 2. WORK-DEFINITION LANGUAGES, PRIORITIZING COMMAND-RUNNING VOCABULARY

## Exact seams in the strongest prior art

### Nushell — structured pipelines, explicit capture of process evidence

**What it got right.** Nushell’s normal pipelines carry structured values, and its `complete` command turns an external process completion into a record containing `stdout`, `stderr`, and `exit_code`. This is much better than pretending a process has one anonymous text result. Sources: https://www.nushell.sh/commands/docs/complete.html; https://www.nushell.sh/book/running_externals.html.

**Where it stops.** Capturing channels into a record does not prove that stdout conforms to an application-specific declared type. An author must still parse/convert the external text, and the correctness of the external tool remains dynamic.

**AWL opportunity.** Treat the process result as a typed boundary object whose raw evidence is always retained, then allow a declared decoder to produce the action’s `T`. The compiler can prove that unvalidated text never enters a `T`; it cannot prove ahead of time that an arbitrary binary will emit valid `T`.

### Oil/YSH — shell reach with more explicit data and argument construction

**What it got right.** Oil/YSH’s project direction is to retain command-language utility while replacing accidental shell semantics with a more regular language and first-class data. It is relevant because it attacks the same author experience: command-oriented work without Bash’s pervasive implicit splitting and string coercion. Primary project sources: https://www.oilshell.org/; https://github.com/oils-for-unix/oils.

**Where it stops.** Compatibility with the Unix command ecosystem necessarily leaves the external program boundary dynamic. The current research did not directly verify one stable YSH syntax page for every array/splicing rule, so AWL should take the design lesson—explicit data and argv—not clone exact syntax without a focused follow-up.

**AWL opportunity.** AWL does not need shell compatibility as a language goal. It can make direct argv execution the only ordinary command primitive and make shell interpretation an explicit, capability-gated escape hatch.

### systemd units — excellent process semantics, weak application semantics

**What it got right.** `ExecStart=` does not implicitly invoke a shell; systemd parses a program and arguments, and shell operators require an explicitly named shell or shell prefix. `Restart=`, `RestartSec=`, stepped backoff, start limits, timeout controls, success-exit classification, and cgroup-wide kill behavior are explicit. The default `KillMode=control-group` terminates all processes in the unit cgroup and escalates from a graceful signal to a final kill after timeout. Sources: https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.service.xml; https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.kill.xml.

**Where it stops.** systemd decides restart from process state, exit status, signal, watchdog, timeout, or OOM. It does not know whether replaying the command is semantically safe. The same restart machinery can rerun a payment command or a cache refresh; idempotence is outside the unit language. Source: https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.service.xml.

**AWL opportunity.** Combine systemd-grade process control with a language-required semantic retry posture. Process supervision and business retry safety become separate, both explicit axes.

### Ansible modules — typed inputs and JSON-shaped results, but arbitrary commands remain arbitrary

**What it got right.** Ansible module argument specifications attach types to inputs. The command module offers mutually exclusive string and `argv: list[string]` forms; `argv` avoids quoting/splitting problems. It returns `stdout`, `stderr`, `rc`, and line forms as explicitly typed result fields. `command` avoids shell metacharacter interpretation; `shell` is a separate, more dangerous primitive. `creates` and `removes` provide limited repeatability/check-mode hints. Source: https://docs.ansible.com/ansible/latest/collections/ansible/builtin/command_module.html.

**Where it stops.** stdout and stderr remain strings, arbitrary command behavior is not inferred, and check mode is only partial. Playbook retries (`until`/`retries`/`delay`) are primarily call-site policy; the command implementation does not carry a mandatory semantic declaration that retries are safe. Retry source: https://docs.ansible.com/ansible/latest/playbook_guide/playbooks_loops.html#retrying-a-task-until-a-condition-is-met.

**AWL opportunity.** Preserve the pragmatic module shape—typed args, structured result, explicit command-vs-shell distinction—but move retry admissibility into the action contract and make output decoding part of the checked worker definition.

### Erlang behaviours — one interface declaration, multiple implementations

**What it got right.** Erlang behaviours define callback obligations; `-callback` specifications describe required callback types, and implementations declare the behaviour. This is a direct precedent for the worker contract as an implementation obligation rather than merely a caller-side declaration. Sources: https://www.erlang.org/doc/system/design_principles.html#behaviours; https://www.erlang.org/doc/reference_manual/typespec.html#behaviours.

**Where it stops.** Behaviour conformance is about callbacks and types. It does not express that a callback invokes a side effect that is safe to retry, requires an idempotency key, needs filesystem/network capabilities, or produces typed stdout from an OS command.

**AWL opportunity.** AWL actions can be “behaviours plus execution semantics”: implementation type, effect posture, capabilities, timeout/cancellation contract, and result decoder in one compiler-owned declaration.

### Nickel, Dhall, CUE, Starlark, and HCL — configuration can be a real language

**What they got right.** Nickel adds contracts to configuration; Dhall emphasizes typed total configuration; CUE treats constraints and values uniformly; Starlark offers a deterministic Python-like configuration/extension language; HCL separates a parseable expression/configuration model from the host application. Sources: https://nickel-lang.org/user-manual/introduction/; https://docs.dhall-lang.org/; https://cuelang.org/docs/concept/the-logic-of-cue/; https://github.com/bazelbuild/starlark/blob/master/spec.md; https://github.com/hashicorp/hcl/blob/main/hclsyntax/spec.md.

**Where they stop.** Their execution meaning is supplied by the embedding application. A field called `retries` or `command` is only as truthful as the host implementation; the language generally does not own the process driver, durable history, or worker registration.

**AWL opportunity.** Keep their values—small grammar, deterministic evaluation, composition, source-spanned contract errors—but use full-stack ownership to guarantee that a work word has one executable meaning across checker, compiler, runtime, UI, and failure handling. That matches AWL’s “no word that lies” law: `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-BIG-PICTURE.md:146-157`.

### CI YAML and markdown procedures for LLMs — the negative controls

**CI YAML.** GitHub Actions `run` steps are shell command strings, and GitHub’s own security guidance warns that interpolating untrusted context into generated shell scripts can cause script injection; it recommends intermediate environment variables or actions instead. Cross-step values are largely strings and runtime expressions. Sources: https://docs.github.com/en/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions#understanding-the-risk-of-script-injections; https://docs.github.com/en/actions/writing-workflows/workflow-syntax-for-github-actions.

**Markdown-for-LLMs.** A markdown procedure is readable but has no compiler-enforced control flow, type contract, retry semantics, or effect boundary. Model behavior also requires evaluation because outputs are probabilistic rather than a deterministic execution of the prose. Primary guidance: https://platform.openai.com/docs/guides/evals; local framing: `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-BIG-PICTURE.md:18-39`.

**AWL opportunity.** Preserve the readability of CI and markdown while moving control flow, types, retries, and evidence into executable semantics. Prose remains narration and documentation, not the mechanism that determines whether work is safe.

## The highest-value command-language opportunities

### A. Required retry posture: `safe`, `never`, `keyed` as semantic categories

The strongest opportunity is to require every side-effecting action to declare a retry posture; the exact spelling can remain open. The three categories named by the owner are a sound opportunity map:

- **safe** — repeating the same logical action is part of the action’s contract;
- **never** — the runtime must not automatically repeat after an ambiguous or failed attempt;
- **keyed** — repetition is admissible only with a stable idempotency/deduplication key.

This belongs primarily on the **action contract**, because retry safety is a property of what the action does, not a preference of one caller. The call site should select timeout, maximum attempts, and backoff only within the action’s declared admissible policy, and should supply/derive the key when the action is keyed. A call site must not be allowed to “upgrade” an action from `never` to retriable. systemd and Kubernetes demonstrate why process/job restart policy alone is insufficient; Kubernetes explicitly warns that a program may start more than once and applications must tolerate duplicates. Sources: https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.service.xml; https://kubernetes.io/docs/concepts/workloads/controllers/job/.

The compiler/runtime combination creates checks unavailable to ordinary SDKs: retries greater than one on `never` are a check error; `keyed` calls without a stable key are a check error; keys can be recorded in durable history and reused on recovery; the console can distinguish “safe retry,” “deduplicated retry,” and “ambiguous—manual decision required.” This is an opportunity, not yet a syntax recommendation.

### B. Typed results parsed and checked from stdout

A useful contract is not “the command returns JSON”; it is “on an accepted exit outcome, decode stdout with decoder D into declared type T, otherwise produce a distinct process/parse failure carrying raw evidence.” Nushell and Ansible provide the raw channels, while AWL can guarantee that only validated `T` crosses into workflow code. Sources: https://www.nushell.sh/commands/docs/complete.html; https://docs.ansible.com/ansible/latest/collections/ansible/builtin/command_module.html; `/Users/tom/Developer/ablative/aion/docs/design/WORKER-AUTHORING-STORY.md:96-112`.

Important limit: the compiler cannot statically prove an arbitrary external executable will emit valid `T`. It can prove that malformed stdout becomes `ParseFailure`, never a forged `T`; that the parser and expected schema agree; and that every caller handles the action’s declared result/failure shape.

### C. argv-array by default; shell is an explicit escape hatch

Direct execution with an argv array removes the shell’s command-substitution, metacharacter, quoting, and accidental word-splitting layer. systemd and Ansible both show the value of “no implicit shell,” and Ansible documents `argv` specifically as the way to avoid quoting problems. Sources: https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.service.xml; https://docs.ansible.com/ansible/latest/collections/ansible/builtin/command_module.html.

This is an injection-killing primitive **for shell injection**, not a universal security proof: a callee may still interpret an argument as an option, path, query, or code. AWL should therefore make argv the ordinary primitive and make `shell` visibly different, capability-gated, and difficult to invoke accidentally.

### D. stderr is failure evidence by default, not a typed business channel

Nushell, Ansible, OTP ports/erlexec, and systemd all treat stdout/stderr as process channels; real programs frequently emit warnings or progress to stderr even on successful exit. Sources: https://www.nushell.sh/commands/docs/complete.html; https://docs.ansible.com/ansible/latest/collections/ansible/builtin/command_module.html; https://www.erlang.org/doc/system/ports.html; https://github.com/saleyn/erlexec.

The safer opportunity is: typed business results normally come from stdout or another explicitly named output artifact; stderr is retained as bounded diagnostic evidence and shown in history. If a tool genuinely defines a structured stderr protocol, AWL may allow an explicit decoder, but making stderr typed by default would confuse a Unix convention with a reliable contract.

### E. Separate process outcome from domain outcome

The worker/runtime should preserve at least these distinct facts: spawn failure, accepted exit, rejected exit code, signal termination, timeout, cancellation, resource-limit kill, output-limit kill, decode failure, and worker loss/ambiguous completion. Collapsing them into `String error` destroys retry and debugging information. systemd’s exit/signal/timeout taxonomy, erlexec’s exit-code/signal reporting, and Kubernetes pod-failure policies demonstrate the value of explicit process outcomes. Sources: https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.service.xml; https://github.com/saleyn/erlexec; https://kubernetes.io/docs/concepts/workloads/controllers/job/.

## What makes AWL feel “I can write it myself and solve complex problems without sacrificing anything”

- **Intent density:** the ordinary case is command + argv + typed input/output + required retry posture, not a page of runner plumbing.
- **No hidden shell:** interpolation produces one argument, not a second parser invocation.
- **Structured composition:** command results become nominal AWL values that flow through existing pipes, decisions, routes, distribute/collect, and subflows. Sources: `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-FLOW-VOCABULARY.md:20-105,107-169`.
- **Honest escape hatches:** an explicit shell action and scaffolding to Python/Gleam remain available when the vocabulary runs out; the escape is visible rather than silently infecting every command. Source: `/Users/tom/Developer/ablative/aion/docs/design/WORKER-AUTHORING-STORY.md:114-132`.
- **Failure is legible:** the author and operator see exit evidence, decoded result, retry reason, key, timing, resource kill, and source span.
- **Every word is end-to-end:** a new worker word must land in lexer, parser, checker, emitter, LSP, tree-sitter grammar, runtime, and UI before it exists. Source: `/Users/tom/Developer/ablative/aion/docs/design/WORKER-AUTHORING-STORY.md:96-112`.
- **Restriction buys guarantees:** no arbitrary ambient I/O or shell interpolation in workflow code; the worker word-set is where effects live. Source: `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-BIG-PICTURE.md:65-72`; `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-2-SPEC.md:33-47,628-639`.

---

# 3. SUPERVISED COMMAND EXECUTION

## Prior-art map

### OTP ports

**What they got right.** A port is owned by an Erlang process, communicates through messages/byte streams, can be linked, and terminates when its owner terminates; the external program is expected to terminate when the port closes. This cleanly brings an OS-process boundary into the actor supervision model. Source: https://www.erlang.org/doc/system/ports.html.

**Where they stop.** The basic port documentation does not guarantee descendant-process cleanup, process-group killing, cgroup containment, hard resource limits, or a typed result. “The external program should terminate” is weaker than a kill-tree guarantee. Source: https://www.erlang.org/doc/system/ports.html.

**AWL opportunity.** Use a port-like ownership relationship internally, but do not expose raw ports as the safety story. The command action needs an OS-process driver with stronger lifecycle containment.

### erlexec

**What it got right.** erlexec demonstrates the missing production features: direct argv execution without a shell, OS PID reporting, stdout/stderr routing, monitoring, process groups, `kill_group`, configurable TERM→KILL escalation, environment and working-directory control, effective-user/capability options, and child cleanup through a dedicated native helper. Source: https://github.com/saleyn/erlexec.

**Where it stops.** It adds a privileged/native helper process and platform-specific operational concerns. It still cannot know whether restarting a managed command is semantically safe or how the result participates in durable replay.

**AWL opportunity.** Treat erlexec as the minimum process-hygiene bar, then add language-level retry posture and durable attempt recording above it.

### systemd

**What it got right.** systemd is the strongest host-level reference here: service restart classification, bounded/stepped backoff, watchdogs, start limits, direct execution, cgroup-wide lifecycle, graceful-stop timeout and forced kill, sandbox directives, and cgroup resource controls. Sources: https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.service.xml; https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.kill.xml; https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.exec.xml; https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.resource-control.xml.

**Where it stops.** It supervises a service/unit, not one durable logical action attempt with a typed result and recorded idempotency key. It cannot distinguish “the process died before charging” from “the charge committed and the acknowledgment was lost.”

**AWL opportunity.** Import systemd’s process semantics into an action-attempt object whose lifecycle is recorded by Aion. Worker restart, command retry, and durable activity retry become three separate concepts instead of one overloaded `Restart=`.

### Nomad exec driver

**What it got right.** Nomad’s exec/raw-exec drivers and job specifications separate task execution from desired-state restart, allocation placement, resources, kill timeouts, and deployment lifecycle. This is the right control-plane decomposition for multi-node command workers. Sources: https://developer.hashicorp.com/nomad/docs/drivers/exec; https://developer.hashicorp.com/nomad/docs/drivers/raw_exec; https://developer.hashicorp.com/nomad/docs/job-specification/restart; https://developer.hashicorp.com/nomad/docs/job-specification/resources.

**Where it stops.** Nomad supervises tasks and allocations; it does not own the workflow language or durable action history, so semantic idempotence and typed command output remain application concerns.

**AWL opportunity.** Aion’s proposed worker deployment model already borrows the desired-state/placement lesson while using content-addressed artifacts and beamr supervision: `/Users/tom/Developer/ablative/aion/docs/design/WORKER-DEPLOYMENT.md:25-45,100-177`.

### Kubernetes Jobs

**What they got right.** Jobs provide completion counts, exponential retry backoff, per-index retry limits, failure policies based on exit code/Pod condition, deadlines, parallelism, and cleanup controls. Sources: https://kubernetes.io/docs/concepts/workloads/controllers/job/.

**Where they stop.** Kubernetes explicitly warns that a program may be started more than once, including duplicate Pods for one index, and applications must tolerate restarts/duplicates. Job retry is therefore not an exactly-once side-effect guarantee. Process/resource semantics are split among Job controller, Pod spec, kubelet, and container runtime. Source: https://kubernetes.io/docs/concepts/workloads/controllers/job/.

**AWL opportunity.** Aion can make the duplicate-attempt reality visible in the language and history instead of leaving it as workload guidance. A keyed action can deduplicate; a safe action can repeat; a never action can enter an explicit ambiguous/manual state.

### Foreman and Overmind

**What they got right.** Procfile managers make local multi-process startup, log multiplexing, and coordinated stop convenient; Overmind adds a tmux-backed developer experience. Sources: https://github.com/ddollar/foreman; https://github.com/DarthSim/overmind.

**Where they stop.** Their documented center of gravity is developer process management, not durable action identity, multi-node desired state, semantic retry contracts, hard tenant isolation, or recorded workflow recovery.

**AWL opportunity.** Steal the immediacy—one file, one command, visible logs—not the reliability model.

## The uniquely available architecture

Owning all four layers permits a separation most systems cannot enforce:

1. **Language layer:** action contract declares typed inputs/result, retry posture, timeout/cancellation semantics, and required capabilities.
2. **Durable-runtime layer:** each logical action and attempt has a durable identity; dispatch, result, timeout, cancellation, key, and ambiguity are recorded. Recovery consults history before doing anything again. Existing replay seam: `/Users/tom/Developer/ablative/aion/docs/design/CONTROL-PLANE.md:145-172`.
3. **Worker-supervision layer:** beamr supervisors keep command-runner workers alive, restart crashed runners with backoff, drain them without accepting new work, and distinguish runner restart from action retry. Proposed substrate: `/Users/tom/Developer/ablative/aion/docs/design/WORKER-DEPLOYMENT.md:179-218,405-443`.
4. **OS-process layer:** each command is direct-spawned into a contained process group/cgroup with bounded channels, resource controls, TERM grace, KILL escalation, and reaping.

That decomposition answers the hardest ambiguity: **restarting a worker process must not automatically mean rerunning its last command.** After restart, the worker asks the durable runtime whether the action completed, is still known to be running, may be retried under its posture, or is ambiguous and requires key/manual handling. Ordinary supervisors do not have the durable history needed to make that decision.

## Minimum bar for a credible command runner

The opportunity should not be marketed as “supervised commands” until the OS-process tier can demonstrate:

- direct argv execution and an explicit shell escape;
- a new process group/session or cgroup per attempt;
- descendant containment and whole-tree termination;
- SIGTERM/cancellation grace followed by SIGKILL of the group;
- child reaping/subreaper behavior so zombies are not leaked;
- bounded stdout/stderr capture with truncation/overflow policy;
- distinct timeout, cancellation, signal, exit, OOM/resource, and parse outcomes;
- CPU, memory, process-count, file-size, and open-file limits where the platform supports them;
- controlled cwd, environment, secrets injection, and filesystem/network capabilities;
- drain semantics that never kill acknowledged in-flight work merely to satisfy scale-down;
- crash tests for parent death, worker death, server death, timeout with grandchildren, output floods, and effect-completed/ack-lost ambiguity.

The repository is candid that BEAM-process isolation is fault isolation, not hostile-code sandboxing, and that hard CPU/memory caps need the proposed OS-process/cgroup tier; a wasm sandbox is a later driver and does not exist today. Source: `/Users/tom/Developer/ablative/aion/docs/design/WORKER-DEPLOYMENT.md:221-290,346-369`.

## The key semantic distinction: restart versus retry versus idempotence

- **Supervisor restart** restores a worker service after process failure.
- **Action retry** starts another attempt for one durable logical action.
- **Idempotence/deduplication** determines whether multiple attempts can safely converge on one external effect.

systemd, Nomad, and Kubernetes are strong at the first two operationally but do not infer the third. AWL can require the third to be declared and checked because it owns the action language and durable scheduler. Sources: https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.service.xml; https://developer.hashicorp.com/nomad/docs/job-specification/restart; https://kubernetes.io/docs/concepts/workloads/controllers/job/.

---

# 4. AFFORDANCES OF OWNING THE WHOLE STACK

## Prior art and the lesson for each option

### Elm — guarantees by removing dangerous expressivity

**What it got right.** Elm’s reliability story is built from language restrictions and types: no ambient exceptions/null in ordinary application code, controlled effects, and compiler-guided refactoring. The lesson is not “copy Elm syntax”; it is that a small language can outperform a general-purpose host on guarantees by making invalid operations inexpressible. Sources: https://guide.elm-lang.org/error_handling/; https://elm-lang.org/news/compilers-as-assistants.

**What can go wrong.** Restriction fails when users must constantly escape to unchecked code. AWL therefore needs a strong boundary: deterministic workflow vocabulary inside, typed action effects outside, and scaffolded native-language workers when the work outgrows AWL. Local policy: `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-BIG-PICTURE.md:146-157`.

**Unique AWL affordance.** Deterministic replay can be a compiler property rather than author guidance. The workflow language already excludes ambient clock, randomness, and I/O and records every dispatch/timer/signal/fork/loop event. Sources: `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-2-SPEC.md:33-47,471-484`.

### Unison — content-addressed code as identity

**What it got right.** Content-addressed definitions make dependency identity immutable and reduce reliance on mutable names. Source: https://www.unison-lang.org/learn/the-big-idea/.

**What can go wrong.** Content hashing identifies code; it does not automatically provide state migration or behavioral compatibility.

**Unique AWL affordance.** Aion already uses content hashes as package versions and can route/pin versions. AWL can put the contract descriptor and source map under the same hash, allowing durable histories and action calls to name exact code/contract identities. Sources: `/Users/tom/Developer/ablative/aion/CLAUDE.md:35`; `/Users/tom/Developer/ablative/aion/crates/aion/tests/engine_reload.rs:327-390`; `/Users/tom/Developer/ablative/aion/crates/aion/tests/deploy_persistence_e2e.rs:466-526`.

### Erlang/OTP hot upgrade — powerful, but operationally expensive

**What it got right.** OTP supports releases, `.appup` upgrade instructions, `code_change`, and coexistence of old/new module versions. It proves live code upgrade is possible in a supervised actor runtime. Sources: https://www.erlang.org/doc/system/release_handling.html; https://www.erlang.org/doc/system/release_structure.html.

**What can go wrong.** True in-place state transformation is application-specific, operationally subtle, and constrained by the VM’s code-version rules. It is not the same as routing new work to a new immutable version while old work drains.

**Unique AWL affordance.** Prefer the cheap, safer subset first: content-addressed side-by-side versions, new starts route to new, in-flight runs remain pinned, old workers drain, compatibility checked at deploy. Treat arbitrary live state migration/`code_change` as an explicit later feature, not the default meaning of “hot upgrade.” The proposed drain design already states this split: `/Users/tom/Developer/ablative/aion/docs/design/WORKER-DEPLOYMENT.md:424-443`.

### Cloudflare Durable Objects — runtime-owned identity, locality, and durable state

**What it got right.** Durable Objects bind a stable object identity to single-threaded coordination and durable storage, giving the platform control over placement and execution around that identity. Source: https://developers.cloudflare.com/durable-objects/what-are-durable-objects/.

**Where it differs.** The model is a durable stateful object, not a replay-checked work language with a compiler-owned activity boundary. It is not direct prior art for source-level deterministic replay or whole workflow/worker contract closure.

**Unique AWL affordance.** Aion can connect durable identity to source-level steps, exact action contracts, event history, and version hashes, not only an object instance.

### Darklang — deployless immediacy and trace-driven development

**What it got right.** Darklang’s core product direction treats editing, execution, deployment, and observed traces as one environment rather than separate build/deploy tooling. Primary project sources: https://docs.darklang.com/; https://github.com/darklang/dark.

**What can go wrong.** A live/deployless experience can blur immutable release identity and reproducibility unless the system records exact versions and histories.

**Unique AWL affordance.** AWL can offer Darklang-like immediacy in the studio while preserving content-addressed packages and pinned durable runs. “Deployless feeling” need not mean mutable anonymous code.

### Smalltalk images — the live system as the development object

**What it got right.** Smalltalk/Pharo images preserve a live object environment and make inspection, debugging, and modification immediate. Primary source: https://books.pharo.org/updated-pharo-by-example/.

**What can go wrong.** Image-state deployment can make provenance, reproducibility, security review, and clean separation of code from durable business history difficult.

**Unique AWL affordance.** Take the live-inspection feeling, not the mutable-image artifact. Aion’s immutable source/package plus event history is a better basis for time travel than serializing an entire mutable VM world.

## Opportunity-by-opportunity feasibility

### 1. Deterministic replay as a language guarantee — **cheap/high confidence**

AWL already excludes ambient workflow I/O/time/randomness and records workflow-visible effects; the checker and MIR have a closed capability surface. The runtime already detects history mismatches and reuses recorded activity results. Sources: `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-2-SPEC.md:33-47,471-504`; `/Users/tom/Developer/ablative/aion/docs/design/CONTROL-PLANE.md:145-172`; `/Users/tom/Developer/ablative/aion/crates/aion-awl/src/mir/runtime.rs`; `/Users/tom/Developer/ablative/aion/crates/aion-awl/src/mir/verify.rs`.

The opportunity is to make this a published compiler guarantee with tests over every workflow construct. The trap is extending worker/effect words into workflow files or allowing untracked dynamic calls.

### 2. Cross-boundary contracts — **medium effort/highest leverage**

AWL-side calls and schemas already exist; worker-AWL can use the same compiler. External workers require generated contract descriptors, codecs/stubs, registration checks, and evolution rules. Sources: `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-2-SPEC.md:233-256`; `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-BIG-PICTURE.md:74-92`.

The trap is claiming semantic proof for external workers. The platform can prove interface compatibility and require declared posture/capabilities; it cannot prove arbitrary Python code is truthful.

### 3. Capability security at language level — **cheap statically, medium-to-expensive operationally**

The MIR already has a closed runtime-capability surface and verifier, and the runtime requests `ExternalIo` capability at a concrete boundary. Sources: `/Users/tom/Developer/ablative/aion/crates/aion-awl/src/mir/runtime.rs`; `/Users/tom/Developer/ablative/aion/crates/aion-awl/src/mir/verify.rs`; `/Users/tom/Developer/ablative/aion/crates/aion/src/runtime/handle.rs:195-211`.

AWL worker actions could compile to a capability manifest—commands, filesystem roots, network destinations, secrets, subprocess, shell—and deployment could compare it with namespace grants. The trap is confusing a checked manifest with enforcement: hostile command isolation and hard limits require the OS-process/cgroup or future wasm driver. Source: `/Users/tom/Developer/ablative/aion/docs/design/WORKER-DEPLOYMENT.md:346-369`.

### 4. Content-addressed hot rollout — **medium/cheap if defined as pin-and-drain; trap if defined as arbitrary state migration**

Content-hash package identity, persisted routes, simultaneous versions, and version-pinning tests already exist. Sources: `/Users/tom/Developer/ablative/aion/CLAUDE.md:35`; `/Users/tom/Developer/ablative/aion/crates/aion/tests/engine_reload.rs`; `/Users/tom/Developer/ablative/aion/crates/aion/tests/deploy_persistence_e2e.rs`; `/Users/tom/Developer/ablative/aion/crates/aion/tests/child_await_e2e.rs:1015`.

A checked rollout can compare old/new action contracts, route new starts to the new hash, keep old runs/workers pinned, and drain before purge. Full OTP-style state transformation should remain separate and explicit.

### 5. Time-travel debugging over durable history — **medium and unusually differentiated**

The runtime already has event-sourced history/replay, the studio already has a scrubber, and AWL syntax/checker nodes carry source spans. Sources: `/Users/tom/Developer/ablative/aion/docs/design/aion-authoring/awl/AWL-BIG-PICTURE.md:41-58,131-133`; `/Users/tom/Developer/ablative/aion/crates/aion-awl/src/lexer/tokens.rs:27-32`; `/Users/tom/Developer/ablative/aion/crates/aion-awl/src/checker/error.rs:6`; `/Users/tom/Developer/ablative/aion/crates/aion-awl/src/parser/exprs.rs:427`.

The cheap version is read-only source-mapped replay: scrub history and highlight the exact AWL step/expression and values. The medium version is fork-from-history or pin-recorded-result into a new run. The trap is mutating canonical history in place, which would undermine replay and auditability.

### 6. Tree-shaken AOT native workers — **strategically unique, not cheap now**

beamr already has type-directed JIT sidecars, Cranelift, stack maps/safepoints, AOT caching, and demand-driven BIF resolution. But the north-star explicitly says whole-program AOT is not near-term, JIT lowering is incomplete, runtime scheduling/GC/supervision must work in native code, and known correctness bugs remain prerequisites. Source: `/Users/tom/Developer/ablative/beamr/docs/AOT-NORTH-STAR.md:1-6,19-34,38-90,123-139`.

The opportunity is real specifically for closed-world AWL workers: the compiler can know reachable actions, capabilities, codecs, and runtime imports, making aggressive tree shaking more tractable than general Erlang/OTP. The trap is using AOT as the reason to design worker-AWL now or putting it ahead of command semantics, worker lifecycle, and VM correctness.

## Cheap wins versus traps

**Cheap/near-term:** deterministic workflow guarantee; AWL↔AWL contract checking; contract descriptors; registration refusal; source-mapped read-only time travel; capability manifests; content-hash pin-and-drain compatibility checks.

**Medium:** truthful polyglot conformance; OS-process command driver; cgroup enforcement; fork-from-history; per-action capability grants; automatic compatible rollout.

**Traps:** claiming exactly-once external effects; implicit shell; treating stderr as typed business output by default; allowing callers to declare an unsafe action safe; arbitrary in-place OTP state migration; Smalltalk-style mutable image deployment; hostile-code claims on BEAM-process isolation; prioritizing whole-program AOT before beamr correctness and full lowering.

---

# Ranked shortlist — five highest-leverage opportunities

## 1. Close the workflow↔worker contract at compile, deploy, and registration

Make one content-addressed action contract drive AWL call checking, AWL-worker body checking, generated external-worker stubs/codecs, compatibility checks, and registration refusal. This is the most defensible “only because we own the whole stack” feature: most competitors have typed calls or IDL, but not one compiler plus the queue registration door and durable version identity. It builds directly on typed worker requirements and content-hash deploys already present. **Single biggest risk:** overstating external-worker guarantees; schema/stub conformance proves shape and declared policy, not arbitrary implementation behavior.

## 2. Make unsafe command retries unwritable through required action posture

Require every side-effecting command action to declare whether repetition is safe, forbidden, or requires a stable key; let call sites choose only bounded timing within that contract. Record keys and attempts in durable history. This attacks the most dangerous seam in durable command execution and turns today’s documentation burden into a checker/runtime invariant. **Single biggest risk:** a three-word taxonomy may be too coarse for real operations—especially partially idempotent actions, compensations, and ambiguous crash windows—so it must grow from fixtures rather than become an over-designed effect system.

## 3. Ship a first-class typed direct-command worker with real kill-tree hygiene

Lift the proven shell-worker manifest into worker-AWL, but raise the semantics: argv-only by default, explicit shell escape, typed stdout decoder, raw stderr evidence, separate process outcomes, process-group/cgroup containment, bounded output, timeout TERM→KILL, reaping, and resource limits. This makes AWL immediately useful for the large class of real work that is “run tools safely,” while the language keeps the surface far smaller than Ansible/systemd/Kubernetes. **Single biggest risk:** mistaking BEAM supervision for OS containment; without the Tier-2 OS-process driver and adversarial kill-tree tests, the feature would promise more than it can enforce.

## 4. Turn deterministic replay into source-mapped time-travel debugging

Attach stable source/node identities to recorded events so the existing scrubber can move through the `.awl` source, show bindings and action evidence, and later fork a new run using selected recorded results. This compounds assets already present—closed deterministic vocabulary, event history, source spans, and studio—into a user-visible advantage that SDK competitors cannot reproduce as cleanly. **Single biggest risk:** state editing or result pinning can corrupt replay/audit semantics if implemented as mutation of canonical history rather than creation of a new, explicitly derived run.

## 5. Make content-addressed rollout a checked pin-and-drain operation

Use existing package hashes and version routing to compare old/new workflow and worker contracts, route new starts to the new version, keep in-flight runs pinned, drain old workers, and refuse incompatible purges. This offers most of the practical value people want from hot upgrade without inheriting the full complexity of OTP `code_change`. **Single biggest risk:** durable data/schema evolution remains harder than code routing; a green interface diff is not sufficient unless persisted payload/history compatibility is also checked.

