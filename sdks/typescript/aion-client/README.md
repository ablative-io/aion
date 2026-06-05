# aion-client-typescript

TypeScript caller SDK for connecting to an `aion-server` deployment and driving workflows from Node.js or browser runtimes that provide `fetch` and a compatible WebSocket transport.

```ts
import { connect, rawPayload } from "aion-client-typescript";

const client = await connect({
  endpoint: "https://aion.example.com",
  namespace: "default",
  auth: {
    bearerToken: process.env.AION_TOKEN,
    subject: "frontend",
    namespaces: ["default"],
  },
  tls: { enabled: true },
});

const handle = await client.start({
  workflowType: "checkout",
  input: { cartId: "cart-123" },
  idempotencyKey: "checkout-cart-123",
});

await handle.signal({
  signalName: "payment_authorized",
  payload: { authorizationId: "auth-1" },
});
const state = await handle.query<{ includeLines: boolean }, { status: string }>(
  {
    queryName: "state",
    input: { includeLines: true },
  },
);

await client.list();
await handle.describe({ includeHistory: true });
await handle.cancel({ reason: "caller requested cancellation" });

const binary = rawPayload(
  new Uint8Array([1, 2, 3]),
  "application/octet-stream",
);
await handle.signalRaw({ signalName: "binary", payload: binary });

for await (const event of handle.subscribe()) {
  console.log(event.seq, event.envelope);
}
```

The SDK exposes typed JSON helpers (`toPayload`/`fromPayload`) and raw `Payload` methods for every payload-bearing operation. Server and transport failures are converted to the branchable `AionClientError` taxonomy.
