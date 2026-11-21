# ==========================================
# Stage 1: Build the release binary
# ==========================================
FROM rust:bookworm AS builder

# Install build dependencies needed for BoringSSL (btls-sys), bindgen, and C/C++ compilation
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    clang \
    libclang-dev \
    llvm-dev \
    build-essential \
    git \
    perl \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy source tree
COPY . .

# Build the high-performance release binary
RUN cargo build --release --bin cfsolve-server

# ==========================================
# Stage 2: Minimal runtime image
# ==========================================
FROM debian:bookworm-slim AS runner

# Install CA certificates for outgoing HTTPS requests
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy release binary and browser profiles
COPY --from=builder /app/target/release/cfsolve-server /usr/local/bin/cfsolve-server
COPY --from=builder /app/profiles /app/profiles

# Default environment configuration
ENV HOST=0.0.0.0
ENV PORT=8080

EXPOSE 8080

# Start cfsolve API server
ENTRYPOINT ["/usr/local/bin/cfsolve-server"]
