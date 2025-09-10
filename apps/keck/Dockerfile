# Build stage
FROM rust:1.75-bullseye as builder

WORKDIR /app

# Copy the workspace configuration
COPY Cargo.toml Cargo.lock ./
COPY apps/keck/Cargo.toml ./apps/keck/
COPY libs/ ./libs/

# Copy source code
COPY apps/keck/src/ ./apps/keck/src/

# Build the application
RUN cargo build --release -p keck

# Runtime stage
FROM debian:bullseye-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create app user
RUN useradd -m -u 1001 keck

# Copy the binary
COPY --from=builder /app/target/release/keck /usr/local/bin/keck

# Set ownership
RUN chown keck:keck /usr/local/bin/keck

# Switch to app user
USER keck

# Expose port
EXPOSE 3000

# Run the application
CMD ["keck"]