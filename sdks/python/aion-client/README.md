# aion-client-python

Async Python caller SDK for Aion workflows. The package is published as `aion-client-python` and imported as `aion_client`.

```python
from aion_client import Client, TLSConfig

async with await Client.connect(
    "https://aion.example.com:443",
    auth="token",
    tls=TLSConfig(enabled=True),
    namespace="payments",
) as client:
    handle = await client.start("invoice", {"invoice_id": "inv_123"}, idempotency_key="start-inv-123")
    await handle.signal("approve", {"by": "ops"})
    state = await handle.query("state", target_type=dict, timeout=5.0)
    workflows = await client.list()
    description = await handle.describe(include_history=True)
    await handle.cancel(reason="caller requested")

    async for event in handle.subscribe():
        print(event)
```

Payload-bearing operations accept JSON values by default and also expose raw bytes via `raw=` and `content_type=`. Errors are branchable subclasses of `AionClientError` matching the shared Aion client taxonomy.
