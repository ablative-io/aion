# C — JSON (the canonical serialization, not an authoring surface)

Every flavour in this folder parses to the same typed graph. **JSON is that graph,
serialized.** It is what the build emits, what the visual editor reads and writes, what
`aion` tooling diffs and migrates — and what no human should ever author by hand.

A fragment (the first three steps of the same workflow):

```json
{
  "workflow": "research_report",
  "version": 1,
  "input": {
    "brief":  { "type": "Brief",  "source": "structured" },
    "corpus": { "type": "Dir",    "source": "content-addressed" }
  },
  "output": "Published",
  "signals": { "review": "Approval" },
  "steps": [
    {
      "id": "questions",
      "kind": "call",
      "action": "plan",
      "args": [ { "ref": "brief" } ]
    },
    {
      "id": "findings",
      "kind": "fan_out",
      "over": { "ref": "questions" },
      "binding": "q",
      "body": {
        "kind": "call",
        "action": "investigate",
        "args": [ { "ref": "q" }, { "ref": "corpus" } ]
      },
      "retry": { "attempts": 3, "backoff": "30s" }
    },
    {
      "id": "draft",
      "kind": "call",
      "action": "synthesize",
      "args": [ { "ref": "findings" }, { "lit": "" } ]
    }
  ]
}
```

…and it continues for ~150 lines. Note what the serialization gets right that YAML-as-
authoring got wrong: `{ "ref": … }` vs `{ "lit": … }` is **explicit** — a machine format
can afford to be verbose about the distinction that YAML left to convention.

## Why this layer exists at all

- **The canvas edits it.** Drag a node, get a one-line JSON diff. (Windmill proved a
  visual workflow can be git-reviewable when the format is a plain canonical file.)
- **Versioning and migration operate on it.** Graph-level diffs ("step `publish` gained a
  compensation") are computed here, not by parsing surface syntax.
- **It is the compatibility boundary.** The authored flavour can evolve cosmetically
  without invalidating stored workflows.

## The cautionary tale that keeps it out of human hands

AWS Step Functions made its JSON (Amazon States Language) the *authoring* surface.
The ecosystem's verdict is unambiguous: thousands-of-line state machines, expressions in
strings (`"$.approval.ok"` JSONPath), and an entire cottage industry of third-party DSLs
(CDK constructs, `stepfunctions-tf`, …) whose only purpose is to let humans avoid writing
it. We skip straight to the ending: humans get flavour A, machines get this.

## Verdict

**Ship it — as the build artifact and canvas format.** Never document it as a thing you
write. `aion` should pretty-print it canonically (stable key order, stable step order) so
diffs stay minimal and merge conflicts stay rare.
