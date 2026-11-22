# ==============================================================================
# cfsolve-server: Fast prebuilt release runner for VPS / Easypanel
# No compilation on VPS: downloads and runs the latest precompiled release binary
# ==============================================================================
FROM ubuntu:24.04

ARG GITHUB_REPO=shinas101/cfsolve
ARG RELEASE_TAG=latest

ENV DEBIAN_FRONTEND=noninteractive

# Install minimal runtime dependencies: TLS certificates, curl, tar, and tzdata (for IANA timezone detection)
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    tar \
    tzdata \
    && rm -rf /var/lib/apt/lists/*

# Set default timezone to UTC and ensure /etc/localtime & /etc/timezone exist
ENV TZ=UTC
RUN ln -snf /usr/share/zoneinfo/UTC /etc/localtime && echo "UTC" > /etc/timezone

WORKDIR /app

# Copy repository profiles directory as local fallback
COPY profiles/ /app/profiles/

# Cache bust to ensure latest binary is fetched on rebuild
ARG CACHEBUST=v0.1.7

# Automatically detect system architecture and download the precompiled release binary
RUN set -eux; \
    ARCH="$(uname -m)"; \
    case "$ARCH" in \
        x86_64)  ASSET_ARCH="x86_64" ;; \
        aarch64|arm64) ASSET_ARCH="aarch64" ;; \
        *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;; \
    esac; \
    if [ "$RELEASE_TAG" = "latest" ]; then \
        DOWNLOAD_URL="https://github.com/${GITHUB_REPO}/releases/latest/download/cfsolve-linux-${ASSET_ARCH}.tar.gz"; \
    else \
        DOWNLOAD_URL="https://github.com/${GITHUB_REPO}/releases/download/${RELEASE_TAG}/cfsolve-linux-${ASSET_ARCH}.tar.gz"; \
    fi; \
    echo "Downloading precompiled cfsolve from ${DOWNLOAD_URL}..."; \
    if curl -f -sSL -o /tmp/cfsolve.tar.gz "$DOWNLOAD_URL"; then \
        tar -xzf /tmp/cfsolve.tar.gz -C /app; \
        rm -f /tmp/cfsolve.tar.gz; \
        if [ -f /app/cfsolve-server ]; then \
            mv /app/cfsolve-server /usr/local/bin/cfsolve-server; \
        fi; \
        if [ -f /app/challenge ]; then \
            mv /app/challenge /usr/local/bin/challenge; \
        fi; \
        if [ -f /app/capture-proxy ]; then \
            mv /app/capture-proxy /usr/local/bin/capture-proxy; \
        fi; \
        chmod +x /usr/local/bin/cfsolve-server 2>/dev/null || true; \
    else \
        echo "========================================================================" >&2; \
        echo "ERROR: Failed to download precompiled release binary from: $DOWNLOAD_URL" >&2; \
        echo "Make sure you have triggered the GitHub Actions 'build' workflow or" >&2; \
        echo "created a release on https://github.com/${GITHUB_REPO}/releases" >&2; \
        echo "========================================================================" >&2; \
        exit 1; \
    fi

ENV HOST=0.0.0.0
ENV PORT=8080

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/cfsolve-server"]
