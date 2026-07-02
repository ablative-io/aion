# ENVIRONMENT.md — Preflight, Repo Acquisition, Workspace Semantics

Read this in full before doing anything else. Do not assume any tool, binary, or path exists — verify every claim in this document with the commands given. If a check fails, tell the operator plainly and stop rather than guessing or working around it silently.

## 1. Workspace semantics (read this twice)

Your file tools (Read/Write/Edit/Glob/Grep — whatever your harness calls them) are **confined to your workspace directory**. They cannot see or touch anything outside it, including the real aion repo checkout at `/Users/tom/Developer/ablative/aion` or any other host path.

Your **shell tool is not confined** the same way. A shell command starts with cwd set to your workspace, but it can `cd`, `cat`, `git clone`, or `cp` anything readable elsewhere on the host, subject to normal filesystem permissions.

Consequences — internalize these patterns:

- To inspect a file that lives outside your workspace (e.g. an operator-given path to an existing aion checkout, or a reference example under `/Users/tom/Developer/ablative/aion/examples/`), use a **shell** `cat`/`ls`/`find`, not your file-read tool. Your file-read tool will simply fail or refuse — that is expected, not a bug.
- To bring outside content into your authored package, `cp` it into your workspace via shell first, then use your normal file tools to edit the copy. Never assume a file tool can reach across the boundary.
- Everything you *author* (the workflow package, `gleam.toml`, source modules, built `.aion` artifact) must end up written inside your workspace via your file tools (or via shell commands whose output lands in-workspace) — that's the only place guaranteed to persist and be reported back correctly.
- If you're unsure whether a path is in-workspace, run `pwd` and `ls` via shell first and compare prefixes. Don't guess.

## 2. Preflight checklist — run this now, verbatim

Run each block. Do not skip any of them and do not assume a result — a tool being on `PATH` in one session does not guarantee it in another.

```bash
# 1. gleam — REQUIRED to build any workflow package. Need >= 1.14.
which gleam && gleam --version
```
- If `gleam` is missing: **stop and tell the operator**: "gleam is not installed on this host; I cannot compile or validate any workflow package. Install gleam >= 1.14 (see https://gleam.run/getting-started/installing/) before I can proceed." Do not attempt to fake compilation or hand-write untested Gleam.
- If present but version `< 1.14`: report the exact version found and ask the operator to upgrade; older gleam may reject syntax the aion SDKs use (check `gleam/aion_flow/gleam.toml` and `gleam/aion_client/gleam.toml` in the repo for the dependency floor once you have the repo — see §3).

```bash
# 2. Erlang runtime — gleam's default target compiles to BEAM and needs erl to run tests/build.
which erl && erl -version 2>&1 | head -1
```
- Missing erl: gleam build/test for `target = "erlang"` packages will fail at the run step (compile may still partially work). Tell the operator; do not silently switch targets.

```bash
# 3. git — needed for repo acquisition (§3) if no local copy is available.
which git && git --version
```
- Missing git: you can still work if the operator supplies files directly, but you cannot clone. Say so.

```bash
# 4. aion binary — used to build/validate/package workflows locally (e.g. `aion build`, `aion package`), and only for `aion deploy` if a server is confirmed reachable (see §5).
which aion && aion --version
```
- Missing aion: you can still author Gleam source, but you cannot build/validate a `.aion` artifact or run local commands. Tell the operator exactly what's missing and what that limits you to (source authoring only, no build/verify step).

```bash
# 5. Network reachability to hex.pm — gleam dependency resolution needs this on first build / any new dependency.
curl -sI --max-time 5 https://hex.pm | head -1
```
- Anything other than a fast `HTTP/2 200` (or timeout/DNS failure): assume no network. `gleam build` will hang or fail on dependency fetch if packages aren't already cached locally. Tell the operator before you burn time on a build that will time out.

Report the full result of this checklist to the operator in your first substantive message if anything is missing or unexpected — don't bury a missing dependency three steps into the task.

## 3. Repo acquisition decision tree

You need the aion repo (or at least `docs/`, `examples/`, `gleam/`) as reference material for authoring — conventions, SDK APIs, example workflows to model against. Work through this in order:

**(a) Workspace is already an aion clone.** Check via shell (not your file tool, until confirmed in-workspace):

```bash
ls docs/ examples/ gleam/ 2>&1
```

If this lists real content (docs like `authoring.md`, `packaging.md`; examples like `hello-world`, `agent-orchestration`; gleam packages `aion_client`, `aion_flow`), you already have it. Proceed — no clone needed. Confirm you're looking at the workspace root, not some unrelated directory, with `pwd` first.

**(b) Operator gave you a local path** (e.g. `/Users/tom/Developer/ablative/aion`). This is outside your workspace — read it with shell commands (`cat`, `ls`, `find`, `grep`), never your file-read tool. Example:

```bash
ls /Users/tom/Developer/ablative/aion/docs /Users/tom/Developer/ablative/aion/examples /Users/tom/Developer/ablative/aion/gleam
cat /Users/tom/Developer/ablative/aion/docs/authoring.md
```

Copy anything you need to reference repeatedly into your workspace via shell `cp`, then switch to file tools for anything you edit.

**(c) Nothing local — clone it.** First confirm network reachability (§2 step 5). The canonical remote, confirmed from this repo's own `.git/config` (`[remote "origin"] url = https://github.com/ablative-io/aion.git`), is:

```bash
git clone --depth 1 https://github.com/ablative-io/aion.git
```

Run this with cwd inside your workspace so the clone lands there. If it fails:
- `Repository not found` / auth prompt → the repo is private or your host has no credentials for it. Tell the operator exactly this: "I cannot reach github.com/ablative-io/aion — it may be private. I need either read access (a token or SSH key already configured on this host) or a local copy of the repo copied into my workspace." Do not guess at a different URL.
- DNS/timeout → no network from this host. Say so plainly; ask the operator to either enable network access or hand you a local path (option b).
- Note: the Gleam SDK packages' own `gleam.toml` files reference `repository = { type = "github", user = "tomWhiting", repo = "aion" }` — this is a personal-fork artifact left in package metadata, not the canonical remote. Trust the `git remote` URL you actually observe, not package metadata, if the two ever disagree.

## 4. Where authored work lives — always report this at the end

Your workspace persists on disk after your session ends (it's run-keyed under the worker's workspace root — you don't need to know the exact host path, just that it survives). This means:

- Write the workflow package (its `gleam.toml`, `src/`, any manifest) somewhere clearly named inside your workspace, e.g. `./<package-name>/`.
- If you build a `.aion` artifact (via `aion build` / `aion package` — verify actual subcommand names with `aion --help` once you've confirmed the binary exists; do not assume flag names), it will land at some path under that same package directory or a build-output directory the command reports — capture that exact path from the command's own output, don't guess it.
- **At the end of every authoring task, regardless of outcome, tell the operator the absolute path (relative-to-workspace path is not enough — give the full path if you can determine it, otherwise the workspace-relative path plus a note that it's under the workspace root) of:**
  1. The authored package directory.
  2. The built `.aion` artifact, if one was produced.
  If you could not build an artifact (e.g. `aion` binary missing, or `gleam build` failed), say that explicitly instead of pretending completion.

## 5. What you can NOT do — be honest about these limits

- **No default access to the aion server API.** You cannot assume there's a running aion server, and you must not invent one. `aion deploy` (or any command that talks to a server) only makes sense if a server URL/target has been explicitly confirmed reachable in this session — check with something like `aion server status` or by asking the operator for the target and testing connectivity (e.g. `curl` its health endpoint) before attempting a deploy. Absent that confirmation, deploys happen through the operator's own console/CLI — your job ends at "here is the built package and artifact path."
- **No secrets.** You have none configured and must not ask the operator to paste any into the conversation, into files, or into environment variables you set. If a workflow needs a secret at runtime, author it to read the secret from the aion runtime's own secret-injection mechanism (check `docs/` for how the SDK expects secrets to be supplied) — never hardcode or request one directly.
- **`OPENAI_API_KEY` is stripped from your environment.** Auth for this session (norn) is OAuth-based, not an API key. If you need to call an LLM from within an authored workflow, that's the *workflow's* concern at runtime (its own configured credentials), not something you provide from your own session environment. Don't try to work around the stripped key by hunting for one elsewhere or asking the operator to supply one "just to test" — treat its absence as a hard boundary, not a bug to route around.

If any of these limits blocks the specific task you're given, say so to the operator immediately and specify exactly what confirmation or access would unblock you, rather than attempting a workaround.
