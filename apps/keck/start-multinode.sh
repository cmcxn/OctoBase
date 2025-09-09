#!/bin/bash

# Local development startup script for multi-node Keck
# This script starts multiple Keck instances for testing

set -e

echo "🚀 Starting Multi-Node Keck for Local Development..."

# Default values
REDIS_URL="${REDIS_URL:-redis://localhost:6379}"
DATABASE_URL="${DATABASE_URL:-postgres://localhost:5432/keck}"
NODE_COUNT="${NODE_COUNT:-3}"
BASE_PORT="${BASE_PORT:-3001}"

echo "📋 Configuration:"
echo "  Redis URL: $REDIS_URL"
echo "  Database URL: $DATABASE_URL"  
echo "  Node Count: $NODE_COUNT"
echo "  Base Port: $BASE_PORT"

# Check if Redis is running
if ! redis-cli ping > /dev/null 2>&1; then
    echo "⚠️  Redis not found. Starting with Docker..."
    docker run -d --name keck-redis -p 6379:6379 redis:7-alpine
    sleep 2
fi

# Array to store PIDs
PIDS=()

# Function to cleanup on exit
cleanup() {
    echo "🛑 Stopping all nodes..."
    for pid in "${PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
    wait
}

# Set up cleanup trap
trap cleanup EXIT INT TERM

# Start multiple Keck nodes
for i in $(seq 1 $NODE_COUNT); do
    PORT=$((BASE_PORT + i - 1))
    NODE_ID="keck-node-$i"
    
    echo "🔄 Starting $NODE_ID on port $PORT..."
    
    KECK_PORT=$PORT \
    NODE_ID=$NODE_ID \
    REDIS_URL=$REDIS_URL \
    DATABASE_URL=$DATABASE_URL \
    RUST_LOG=info \
    cargo run --release -p keck &
    
    PID=$!
    PIDS+=($PID)
    echo "✅ Started $NODE_ID (PID: $PID)"
    
    # Small delay between starts
    sleep 2
done

echo ""
echo "🎉 All nodes started successfully!"
echo ""
echo "📡 Active nodes:"
for i in $(seq 1 $NODE_COUNT); do
    PORT=$((BASE_PORT + i - 1))
    echo "  - Node $i: http://localhost:$PORT (ws://localhost:$PORT/collaboration/{roomid})"
done

echo ""
echo "📚 Testing commands:"
echo "  curl http://localhost:$BASE_PORT/api/workspace/test/blob/test"
echo "  websocat ws://localhost:$BASE_PORT/collaboration/test-room"
echo ""
echo "🔍 Monitoring:"
echo "  redis-cli monitor"
echo "  ./test-multinode.sh"
echo ""
echo "Press Ctrl+C to stop all nodes..."

# Wait for all background jobs
wait