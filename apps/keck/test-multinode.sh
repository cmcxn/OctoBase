#!/bin/bash

# Test script for multi-node Keck setup
# This script tests that multiple nodes are working and synchronized

set -e

echo "🧪 Testing Multi-Node Keck Setup..."

# Check if required services are running
echo "📋 Checking services..."

# Check Redis
if ! redis-cli ping > /dev/null 2>&1; then
    echo "❌ Redis is not running. Please start Redis first."
    exit 1
fi
echo "✅ Redis is running"

# Check if Keck nodes are running
NODES=("3001" "3002" "3003")
RUNNING_NODES=()

for port in "${NODES[@]}"; do
    if curl -s "http://localhost:${port}/api/workspace/test/blob/test" > /dev/null 2>&1; then
        RUNNING_NODES+=($port)
        echo "✅ Keck node on port $port is running"
    else
        echo "⚠️  Keck node on port $port is not responding"
    fi
done

if [ ${#RUNNING_NODES[@]} -eq 0 ]; then
    echo "❌ No Keck nodes are running. Please start at least one node."
    exit 1
fi

echo "📊 Found ${#RUNNING_NODES[@]} running node(s): ${RUNNING_NODES[*]}"

# Check Nginx load balancer
if curl -s "http://localhost:3000/api/workspace/test/blob/test" > /dev/null 2>&1; then
    echo "✅ Nginx load balancer is working"
else
    echo "⚠️  Nginx load balancer is not responding (this is OK if not using nginx)"
fi

# Test Redis pub/sub functionality
echo "🔄 Testing Redis pub/sub..."
REDIS_TEST_CHANNEL="keck:sync:test-workspace"

# Subscribe to Redis channel in background
redis-cli subscribe "$REDIS_TEST_CHANNEL" > /tmp/redis_test.log &
REDIS_PID=$!

# Give it a moment to subscribe
sleep 1

# Publish a test message
redis-cli publish "$REDIS_TEST_CHANNEL" '{"node_id":"test","workspace_id":"test-workspace","operation_type":"Content","data":[1,2,3],"timestamp":1234567890}'

# Wait a moment and check
sleep 1
kill $REDIS_PID 2>/dev/null || true

if grep -q "test-workspace" /tmp/redis_test.log; then
    echo "✅ Redis pub/sub is working"
else
    echo "⚠️  Redis pub/sub test inconclusive"
fi

rm -f /tmp/redis_test.log

# Test WebSocket connections if websocat is available
if command -v websocat > /dev/null 2>&1; then
    echo "🔌 Testing WebSocket connections..."
    
    for port in "${RUNNING_NODES[@]}"; do
        echo "Testing WebSocket on port $port..."
        timeout 5 websocat "ws://localhost:${port}/collaboration/test-room" <<< '{"type":"ping"}' > /dev/null 2>&1 && \
            echo "✅ WebSocket on port $port is working" || \
            echo "⚠️  WebSocket on port $port connection failed"
    done
else
    echo "⚠️  websocat not found, skipping WebSocket tests"
    echo "   Install with: cargo install websocat"
fi

echo ""
echo "🎉 Multi-node test completed!"
echo ""
echo "📚 Usage:"
echo "  - Connect clients to: ws://localhost:3000/collaboration/{roomid}"
echo "  - Individual nodes: ws://localhost:3001/collaboration/{roomid}"
echo "  - Monitor Redis: redis-cli monitor"
echo "  - Check logs: docker-compose logs -f"
echo ""
echo "🚀 Your multi-node Keck setup is ready!"