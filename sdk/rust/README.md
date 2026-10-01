# zcashname-sdk

A small async Rust client for the current ZNS resolver JSON-RPC API. It reads
resolver data; it does not construct or sign name actions.

```toml
[dependencies]
zcashname-sdk = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust,no_run
use zcashname_sdk::ResolverClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let resolver = ResolverClient::new("https://resolver.example/rpc");
    if let Some(record) = resolver.resolve_name("alice").await? {
        println!("alice resolves to {}", record.address);
    }
    let status = resolver.status().await?;
    println!("synced: {} at {}", status.synced, status.synced_height);
    Ok(())
}
```

The client also exposes `resolve`, `list_names`, `reverse_lookup`, and
`events`. Types follow the resolver's JSON field names (`last_action`,
`since_height`, and `action_index`). `expires_at` is `"none"` or a decimal Unix
timestamp.
