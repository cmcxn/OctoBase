# Keck Multi-Node Setup

This guide explains how to set up Keck in a multi-node configuration with Redis synchronization for high availability and scalability.

## Architecture

```
Client Apps
     |
     v
 Nginx (Port 3000)
     |
     v
Load Balancer
     |
     +-- Keck Node 1 (Port 3001)
     +-- Keck Node 2 (Port 3002)  
     +-- Keck Node 3 (Port 3003)
           |
           v
    Redis (Sync) + PostgreSQL (Storage)
```

## Features

- **Multi-node collaboration**: Multiple Keck instances can run simultaneously
- **Redis synchronization**: Real-time sync of CRDT operations between nodes  
- **Load balancing**: Nginx distributes WebSocket connections across nodes
- **Session consistency**: Same roomid gets consistent data across all nodes
- **High availability**: If one node fails, others continue to serve requests

## Quick Start with Docker Compose

1. **Start all services**:
   ```bash
   docker-compose up -d
   ```

2. **Access the application**:
   - Main endpoint: `ws://localhost:3000/collaboration/{roomid}`
   - Individual nodes: `ws://localhost:3001/collaboration/{roomid}`, etc.

3. **Monitor logs**:
   ```bash
   docker-compose logs -f keck-node-1
   ```

4. **Scale up/down**:
   ```bash
   docker-compose up -d --scale keck-node-1=2
   ```

## Manual Setup

### Prerequisites

- Redis server
- PostgreSQL database
- Rust 1.75+

### Environment Variables

Set these variables for each Keck node:

```bash
# Required
DATABASE_URL=postgres://user:password@host:5432/database
REDIS_URL=redis://localhost:6379

# Optional
KECK_PORT=3000              # Port for this node
NODE_ID=keck-node-1         # Unique identifier for this node
HOOK_ENDPOINT=              # Webhook URL for block changes
```

### Running Multiple Nodes

1. **Start Redis**:
   ```bash
   redis-server
   ```

2. **Start PostgreSQL**:
   ```bash
   # Make sure PostgreSQL is running and database is created
   createdb keck
   ```

3. **Start Keck nodes**:
   ```bash
   # Node 1
   KECK_PORT=3001 NODE_ID=node1 REDIS_URL=redis://localhost:6379 \
   DATABASE_URL=postgres://user:pass@localhost/keck cargo run

   # Node 2  
   KECK_PORT=3002 NODE_ID=node2 REDIS_URL=redis://localhost:6379 \
   DATABASE_URL=postgres://user:pass@localhost/keck cargo run

   # Node 3
   KECK_PORT=3003 NODE_ID=node3 REDIS_URL=redis://localhost:6379 \
   DATABASE_URL=postgres://user:pass@localhost/keck cargo run
   ```

4. **Start Nginx**:
   ```bash
   nginx -c /path/to/nginx.conf
   ```

## Configuration

### Nginx Load Balancer

The included `nginx.conf` uses `ip_hash` for session affinity. Edit the upstream block to add/remove nodes:

```nginx
upstream keck_nodes {
    ip_hash;
    server 127.0.0.1:3001 weight=1;
    server 127.0.0.1:3002 weight=1;
    server 127.0.0.1:3003 weight=1;
}
```

### Redis Configuration

For production, consider Redis clustering or Redis Sentinel for high availability:

```bash
# Redis with persistence
redis-server --appendonly yes --save 60 1000

# Redis with password
redis-server --requirepass your-password
```

### Database Migrations

Keck automatically runs database migrations on startup. For production, you may want to run migrations separately:

```bash
# Run migrations before starting nodes
DATABASE_URL=postgres://... cargo run --bin migrations
```

## Testing Multi-Node Setup

1. **Connect to the load balancer**:
   ```javascript
   const ws = new WebSocket('ws://localhost:3000/collaboration/test-room');
   ```

2. **Verify synchronization**:
   - Connect multiple clients to the same room
   - Make changes and verify they appear on all clients
   - Stop one node and verify others continue working

3. **Monitor Redis activity**:
   ```bash
   redis-cli monitor
   ```

## Troubleshooting

### Connection Issues

- Check if all services are running: `docker-compose ps`
- Verify Redis connectivity: `redis-cli ping`
- Check PostgreSQL connectivity: `psql -h localhost -p 5432 -U keck -d keck`

### Synchronization Issues

- Check Redis logs for pub/sub activity
- Verify NODE_ID is unique for each instance
- Ensure all nodes use the same Redis instance

### Performance Issues

- Monitor Redis memory usage: `redis-cli info memory`
- Check PostgreSQL connection pool settings
- Adjust Nginx worker processes and connections

## Production Considerations

1. **Security**:
   - Use TLS for Redis connections
   - Enable PostgreSQL SSL
   - Configure Nginx with SSL/TLS

2. **Monitoring**:
   - Add health checks for each service
   - Monitor Redis pub/sub lag
   - Track PostgreSQL connection counts

3. **Scaling**:
   - Consider Redis Cluster for large deployments
   - Use read replicas for PostgreSQL
   - Implement proper session affinity strategies

4. **Backup**:
   - Regular PostgreSQL backups
   - Redis AOF/RDB persistence
   - Monitor backup integrity

## API Endpoints

- `GET /api/workspace/:id/blob/:name` - Get blob
- `PUT /api/workspace/:id/blob` - Upload blob  
- `POST /collaboration/:workspace` - Auth WebSocket
- `GET /collaboration/:workspace` - Upgrade to WebSocket

All endpoints are automatically load balanced across available nodes.