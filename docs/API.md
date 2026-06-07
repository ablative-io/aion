# Aion HTTP API Reference

This reference describes the HTTP/JSON conventions used by the Aion server and examples. The hello-world dev server listens on `http://127.0.0.1:8080` when started with the repo-root `dev-config.json`.

## Request format

- Send JSON request bodies with `Content-Type: application/json`.
- Namespace-scoped endpoints include a `namespace` field in the request body or query string.
- In shared-engine mode, the requested namespace must be present in `x-aion-namespaces` before the server will call the engine.

## Headers

| Header | Required? | Valid values | Purpose |
| --- | --- | --- | --- |
| `Authorization` | Required when server auth is enabled. | `Bearer <token>`. The hello-world dev config expects `Bearer dev-token`. The current server compares the bearer value exactly to the configured token. | Authenticates the request before namespace grants are accepted. Missing values fail with `missing Authorization header with Bearer token`; wrong or expired values fail with `invalid or expired bearer token`. |
| `x-aion-subject` | Required for authenticated HTTP requests. | Any non-empty caller identifier, for example `hello-world-user` or `alice`. | Identifies the caller in namespace-denial messages and request metadata. Empty or absent values are invalid when auth is enabled. |
| `x-aion-namespaces` | Required for namespace-scoped authenticated requests that must access an engine namespace. | A comma-separated list of namespace names, for example `default` or `tenant-a,tenant-b`. Whitespace around commas is trimmed; empty entries are ignored. | Advertises the namespaces the caller may access. The request's `namespace` value must match one entry; otherwise the server returns `subject not authorized for namespace <name>`. |
| `Content-Type` | Required for JSON request bodies. | `application/json`. | Tells the server to parse the request body as JSON. |

## Example

```sh
curl -sS -X POST http://127.0.0.1:8080/workflows/list \
  -H 'content-type: application/json' \
  -H 'authorization: Bearer dev-token' \
  -H 'x-aion-subject: hello-world-user' \
  -H 'x-aion-namespaces: default' \
  --data '{"namespace":"default"}'
```

In that request, `x-aion-namespaces: default` authorizes the body namespace `default`. To request another namespace, change both the body `namespace` value and include that namespace in the comma-separated header.

## Auth and namespace errors

Authentication and namespace failures are returned as namespace-denied wire errors by the current HTTP adapter. The message identifies what to fix:

- Missing `Authorization`: add an `Authorization: Bearer <token>` header.
- Invalid bearer token: refresh or correct the token and keep the `Bearer ` prefix.
- Missing `x-aion-subject`: send a non-empty caller identifier.
- Namespace mismatch: add the requested namespace to `x-aion-namespaces` or request a namespace already listed there.
