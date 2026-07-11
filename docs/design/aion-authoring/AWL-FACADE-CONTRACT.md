# AWL HTTP facade contract

This document is the client/server contract for the P0 visual authoring surface.
JSON uses UTF-8. Source lines and columns are one-based; byte offsets are
zero-based and `end` is exclusive.

## `POST /awl/check`

Request:

```json
{ "source": "workflow example\n", "path": "flows/example.awl" }
```

`path` is a string or `null`. When present, its parent directory is used to
resolve schema imports. A successful HTTP request returns status 200 even when
the language document has errors:

```json
{
  "ok": true,
  "deploys_green": true,
  "steps": 1,
  "diagnostics": [],
  "semantic": { "entries": [] }
}
```

`ok` means there are no parser or checker errors. `steps` and `semantic` are
non-null only when `ok` is true. `deploys_green` means `ok` is true and the
current Gleam stopgap emitter accepted the checked document. Diagnostics have
shape:

```json
{ "class": "error", "message": "...", "line": 4, "column": 7 }
```

`class` is `error` for parser/checker diagnostics or `emit_subset` for a checked
construct refused by the emitter. Messages are the language tool's verbatim
message (without an added location suffix). Emit output is discarded.

### Semantic index

`semantic.entries` is an array of facts indexed by source span:

```json
{
  "span": { "start": 20, "end": 24, "line": 2, "column": 8 },
  "type": "String",
  "declaration": {
    "name": "name",
    "kind": "input",
    "documentation": null,
    "span": { "start": 20, "end": 24, "line": 2, "column": 8 }
  }
}
```

`type` and `declaration` are independently nullable. `kind` is one of
`workflow`, `input`, `signal`, `outcome`, `type`, `field`, `variant`, `worker`,
`action`, `child`, `parameter`, `step`, or `binding`. `documentation` is the
normalized declaration doc text or null. For hover, select the narrowest entry
whose span contains the cursor byte offset. For go-to-definition, use
`declaration.span`.

## `POST /awl/fmt`

Request `{ "source": string }`. Success is 200:

```json
{ "formatted": "canonical AWL text" }
```

A parse failure is 422 with the same diagnostic object shape:

```json
{ "diagnostics": [{ "class": "error", "message": "...", "line": 1, "column": 1 }] }
```

## Workspace documents

These routes exist only when `[authoring].workspace_dir` is configured;
otherwise every document route returns plain 404. Paths are relative to that
root, must end in `.awl`, and may contain only normal relative components.
Absolute paths and `..` are rejected with status 400 and a typed wire error.
Symlinks are neither traversed by listing nor accepted as writable parents.

- `GET /awl/documents` returns a recursively discovered, path-sorted array of
  `{ "path": "relative/file.awl", "name": "file" }`.
- `GET /awl/documents/{path}` returns `{ "source": string }`; a missing file is
  404 with error type `DocumentNotFound`.
- `PUT /awl/documents/{path}` accepts `{ "source": string }`, creates real parent
  directories inside the root, atomically replaces the file using a temporary
  sibling plus rename, and returns `{ "source": string }`.

Invalid paths use error type `InvalidDocumentPath` (400). Workspace I/O faults
use `DocumentIoError` (500). All typed errors carry the standard Aion
`WireError` JSON body and a meaningful message.
