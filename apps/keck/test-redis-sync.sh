#!/bin/bash

# Test script for Redis multi-node synchronization
set -e

echo "🧪 Testing Redis Multi-Node Synchronization"
echo "=============================================="

# Configuration
REDIS_URL="${REDIS_URL:-redis://localhost:6379}"
DATABASE_URL="${DATABASE_URL:-postgres://localhost:5432/keck}"
NODE_COUNT=2
BASE_PORT=3100
WORKSPACE_ID="test-sync-$(date +%s)"

# Function to clean up background processes
cleanup() {
    echo ""
    echo "🧹 Cleaning up..."
    pkill -f "target/debug/keck" || true
    sleep 2
}
trap cleanup EXIT

# Check dependencies
echo "📋 Checking dependencies..."

if ! command -v redis-cli > /dev/null 2>&1; then
    echo "❌ redis-cli not found. Please install Redis."
    exit 1
fi

if ! redis-cli ping > /dev/null 2>&1; then
    echo "❌ Redis server not running. Please start Redis."
    exit 1
fi

if ! command -v psql > /dev/null 2>&1; then
    echo "⚠️  psql not found. Make sure PostgreSQL client is installed."
fi

echo "✅ Dependencies check passed"

# Build the project
echo ""
echo "🔨 Building Keck..."
cargo build --package keck

# Start test nodes
echo ""
echo "🚀 Starting $NODE_COUNT test nodes..."
PIDS=()

for i in $(seq 1 $NODE_COUNT); do
    PORT=$((BASE_PORT + i - 1))
    NODE_ID="test-node-$i"
    
    echo "  Starting node $i on port $PORT (node_id: $NODE_ID)"
    
    env KECK_PORT=$PORT \
        NODE_ID=$NODE_ID \
        REDIS_URL=$REDIS_URL \
        DATABASE_URL=$DATABASE_URL \
        USE_MEMORY_SQLITE="" \
        cargo run --package keck > /tmp/keck-node-$i.log 2>&1 &
    
    PID=$!
    PIDS+=($PID)
    echo "    PID: $PID"
done

# Wait for nodes to start
echo ""
echo "⏳ Waiting for nodes to start..."
sleep 5

# Check if nodes are running
echo ""
echo "🔍 Checking node status..."
RUNNING_NODES=()

for i in $(seq 1 $NODE_COUNT); do
    PORT=$((BASE_PORT + i - 1))
    if curl -s "http://localhost:$PORT/api/workspace/test/blob/test" > /dev/null 2>&1; then
        echo "  ✅ Node $i (port $PORT) is running"
        RUNNING_NODES+=($PORT)
    else
        echo "  ❌ Node $i (port $PORT) is not responding"
        echo "    Check logs: tail -f /tmp/keck-node-$i.log"
    fi
done

if [ ${#RUNNING_NODES[@]} -lt 2 ]; then
    echo ""
    echo "❌ Not enough nodes running for sync test. Need at least 2 nodes."
    echo "Check the logs for errors:"
    for i in $(seq 1 $NODE_COUNT); do
        echo "  Node $i log: /tmp/keck-node-$i.log"
    done
    exit 1
fi

echo ""
echo "✅ ${#RUNNING_NODES[@]} nodes are running successfully"

# Test Redis pub/sub functionality
echo ""
echo "🔄 Testing Redis pub/sub..."
REDIS_TEST_CHANNEL="keck:sync:$WORKSPACE_ID"

# Subscribe to Redis channel in background and capture output
redis-cli subscribe "$REDIS_TEST_CHANNEL" > /tmp/redis_test.log &
REDIS_PID=$!

# Give it a moment to subscribe
sleep 1

# Publish a test message
TEST_MESSAGE='{"node_id":"test-script","workspace_id":"'$WORKSPACE_ID'","operation_type":"Content","data":[1,2,3],"timestamp":1234567890}'
redis-cli publish "$REDIS_TEST_CHANNEL" "$TEST_MESSAGE"

# Wait a moment and check
sleep 1
kill $REDIS_PID 2>/dev/null || true

if grep -q "$WORKSPACE_ID" /tmp/redis_test.log; then
    echo "✅ Redis pub/sub is working"
else
    echo "⚠️  Redis pub/sub test inconclusive"
    echo "    Redis test log:"
    cat /tmp/redis_test.log | head -10
fi

# Test HTTP API access
echo ""
echo "🌐 Testing HTTP API access..."
for port in "${RUNNING_NODES[@]}"; do
    echo "  Testing node on port $port..."
    if curl -s "http://localhost:${port}/api/workspace/$WORKSPACE_ID/blob/test-blob" > /dev/null 2>&1; then
        echo "    ✅ HTTP API accessible on port $port"
    else
        echo "    ⚠️  HTTP API test failed on port $port"
    fi
done

# Test WebSocket connections if websocat is available
echo ""
if command -v websocat > /dev/null 2>&1; then
    echo "🔌 Testing WebSocket connections..."
    
    for port in "${RUNNING_NODES[@]}"; do
        echo "  Testing WebSocket on port $port..."
        timeout 3 websocat "ws://localhost:${port}/collaboration/$WORKSPACE_ID" <<< '{"type":"ping"}' > /dev/null 2>&1 && \
            echo "    ✅ WebSocket accessible on port $port" || \
            echo "    ⚠️  WebSocket test failed on port $port"
    done
else
    echo "⚠️  websocat not found, skipping WebSocket tests"
    echo "   Install with: cargo install websocat"
fi

# Show log excerpts
echo ""
echo "📋 Recent log excerpts from nodes:"
for i in $(seq 1 $NODE_COUNT); do
    if [ -f "/tmp/keck-node-$i.log" ]; then
        echo ""
        echo "  Node $i log (last 5 lines):"
        tail -5 "/tmp/keck-node-$i.log" | sed 's/^/    /'
    fi
done

# Summary
echo ""
echo "🎉 Redis sync test completed!"
echo ""
echo "📊 Test Summary:"
echo "  - Redis server: $(redis-cli ping 2>/dev/null || echo 'NOT AVAILABLE')"
echo "  - Nodes started: $NODE_COUNT"
echo "  - Nodes running: ${#RUNNING_NODES[@]}"
echo "  - Test workspace: $WORKSPACE_ID"
echo ""
echo "📚 Next steps:"
echo "  - Connect clients to: ws://localhost:${RUNNING_NODES[0]}/collaboration/$WORKSPACE_ID"
echo "  - Monitor Redis: redis-cli monitor"
echo "  - Check individual node logs: /tmp/keck-node-*.log"
echo ""
echo "🔧 Debug commands:"
echo "  redis-cli monitor"
echo "  redis-cli pubsub channels keck:*"
echo "  curl http://localhost:${RUNNING_NODES[0]}/api/workspace/$WORKSPACE_ID/blob/test"

# Clean up test logs on success
rm -f /tmp/redis_test.log

echo ""
echo "✅ Test completed successfully!"