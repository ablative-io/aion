# JSON payload round-trip parity

The worker SDKs carry activity inputs, results, failure details, and heartbeat progress as AW `Payload` values. This parity check verifies that Rust, Python, and TypeScript agree on the JSON baseline content-type tag and can decode each other's JSON encodings without losing fields.

The source of truth for Rust/core semantics is `aion-core::Payload`: JSON payloads are UTF-8 JSON bytes tagged as JSON. On the wire this suite normalizes that tag as `application/json`, matching the conformance scenario vocabulary and the AW `Payload.content_type` string field.

## Representative values

Each SDK must encode and decode this fixed set:

```json
[
  null,
  true,
  false,
  0,
  123.45,
  "hello worker",
  "unicode: café, 東京, 🚀",
  [null, false, 7, "item", {"nested": true}],
  {
    "message": "nested object",
    "count": 3,
    "flags": [true, false],
    "inner": {
      "empty": null,
      "unicode": "snowman ☃"
    }
  }
]
```

## Per-SDK encode/decode contract

For every value above, each SDK emits a normalized payload record:

```json
{
  "sdk": "rust",
  "case": "nested-object",
  "contentType": "application/json",
  "jsonText": "{\"count\":3,\"flags\":[true,false],\"inner\":{\"empty\":null,\"unicode\":\"snowman ☃\"},\"message\":\"nested object\"}",
  "decoded": {
    "message": "nested object",
    "count": 3,
    "flags": [true, false],
    "inner": {"empty": null, "unicode": "snowman ☃"}
  }
}
```

The SDK implementation must use public codec APIs when present:

- Rust: `aion-worker` typed activity codec helpers, which delegate to `aion-core::Payload` JSON semantics.
- Python: the AR-008 activity codec when available; until then, a runner may construct `common_pb2.Payload(content_type="application/json", bytes=...)` directly for parity only.
- TypeScript: the AR-010 JSON activity codec when available; until then, a runner may construct the public `Payload { contentType, bytes }` shape directly for parity only.

If a public JSON codec is unavailable in a local checkout, the parity runner emits one skip line for that SDK and case set:

```text
SKIP sdk=python reason="JSON activity codec unavailable"
```

## Normalization algorithm

Encoded JSON bytes are compared modulo insignificant whitespace and object member order:

1. Assert `contentType === "application/json"`. Any other tag is an immediate failure, even if the bytes contain valid JSON.
2. Decode the bytes as UTF-8.
3. Parse as JSON.
4. Recursively canonicalize objects by sorting keys lexicographically. Arrays keep order. Numbers, booleans, strings, and null keep their JSON values.
5. Re-serialize the canonical value with no insignificant whitespace.
6. Compare the canonical value and canonical string with the expected representative value.

The check fails if an SDK alters the content-type tag, emits invalid UTF-8, emits invalid JSON, loses a field, changes an array order, changes a number or boolean, or changes Unicode content.

## Cross-decode matrix

Each SDK must decode every other SDK's encoding for every representative value:

| Encoded by \\ Decoded by | Rust | Python | TypeScript |
| --- | --- | --- | --- |
| Rust | required | required | required |
| Python | required | required | required |
| TypeScript | required | required | required |

For each cell, the decoded value must equal the canonical representative value and the content-type tag must remain `application/json`. The check is intentionally all-to-all so a one-language serialization quirk, such as changing a string field to bytes or dropping `null`, is caught by the other SDKs.

## Output and failure format

A successful parity runner emits one normalized line per SDK and case:

```text
AION_WORKER_PAYLOAD_PARITY sdk=typescript case=unicode result={"contentType":"application/json","canonical":"\"unicode: café, 東京, 🚀\""}
```

A cross-decode mismatch reports encoder SDK, decoder SDK, case, field path, expected value, and actual value:

```text
DIVERGENCE payload encoder=python decoder=rust case=nested-object path=inner.unicode expected="snowman ☃" actual=null
```

A content-type mismatch is reported before JSON comparison:

```text
DIVERGENCE payload encoder=typescript decoder=python case=array path=contentType expected="application/json" actual="text/json"
```

## Deliberate negative checks

The parity harness should include two negative fixtures in its own self-test:

- Change one SDK record's content type from `application/json` to another string and assert the diff points at `contentType`.
- Remove one field from the nested object and assert the diff points at the missing field path.

These negative checks validate that the parity suite fails loudly when an SDK silently changes content type or loses data on round-trip.
