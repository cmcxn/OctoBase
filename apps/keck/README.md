# Keck

This service provides collaborative Yjs document sync with optional Redis Pub/Sub
to coordinate multiple instances.

## Multi-node synchronization

1. Start a Redis server.
2. For each `keck` instance set the `REDIS_URL` environment variable pointing to
the Redis server, e.g.
   ```bash
   REDIS_URL=redis://127.0.0.1:6379 cargo run -p keck
   ```
3. Run multiple `keck` processes. Clients can connect to any instance using
   `ws://<host>:<port>/collaboration/{roomid}` and will share the same Yjs data.

## Environment variables

- `DATABASE_URL` – database for persistence.
- `REDIS_URL` – enables Redis Pub/Sub for cross-node updates. When unset the
  instance behaves as a single node.

## Tests

A Redis server is required to run the multi-node integration test:

```bash
redis-server --daemonize yes
cargo test -p keck --test-threads=1 -- --ignored
```
