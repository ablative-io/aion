/**
 * Caller identity forwarded to the server on every request: the HTTP
 * transport sends these as request headers and the WebSocket transport
 * sends them on the upgrade request.
 */
export interface AuthOptions {
  readonly bearerToken?: string;
  readonly subject?: string;
  readonly namespaces?: readonly string[];
}

/**
 * Builds the authentication headers shared by the HTTP transport and the
 * WebSocket subscription transport (`authorization`, `x-aion-subject`,
 * `x-aion-namespaces`).
 */
export function authHeaders(auth?: AuthOptions): Record<string, string> {
  const headers: Record<string, string> = {};
  if (auth?.bearerToken !== undefined) {
    headers.authorization = `Bearer ${auth.bearerToken}`;
  }
  if (auth?.subject !== undefined) {
    headers["x-aion-subject"] = auth.subject;
  }
  if (auth?.namespaces !== undefined) {
    headers["x-aion-namespaces"] = auth.namespaces.join(",");
  }
  return headers;
}
