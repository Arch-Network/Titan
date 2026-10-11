# Stage 1: Builder – compile Titan
# RocksDB is compiled from source by the rocksdb crate (bundled), so no system librocksdb is needed.
FROM rust:1.94.0-bookworm AS builder

RUN apt-get update && apt-get install -y \
    build-essential \
    libssl-dev \
    pkg-config \
    libclang-dev \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /tmp
COPY . .
RUN cargo build --locked --release --package titan

# Stage 2: Runner – minimal runtime
FROM debian:bookworm-slim AS runner

# Install runtime deps
RUN apt-get update && apt-get install -y \
    curl \
    libssl3 \
    ca-certificates \
    gosu \
    util-linux \
    passwd \
  && rm -rf /var/lib/apt/lists/*

# Create unprivileged titan user
RUN useradd -ms /bin/bash titan

# Create data directory
RUN mkdir -p /home/titan/data \
  && chown titan:titan /home/titan/data

# Copy the compiled binary
COPY --from=builder --chown=titan:titan /tmp/target/release/titan /usr/local/bin/titan
RUN chmod +x /usr/local/bin/titan

WORKDIR /home/titan

# Default configuration (overridable at runtime). Titan reads every option from the
# environment variable of the same name (e.g. --bitcoin-rpc-url <-> BITCOIN_RPC_URL),
# so no configuration is passed on the command line. BITCOIN_RPC_PASSWORD has no
# default and must be provided at runtime.
ENV DATA_DIR=/home/titan/data \
    COMMIT_INTERVAL=5 \
    BITCOIN_RPC_URL=http://bitcoind:18443 \
    BITCOIN_RPC_USERNAME=bitcoin \
    CHAIN=regtest \
    HTTP_LISTEN=0.0.0.0:3030 \
    TCP_ADDRESS=0.0.0.0:8080 \
    MAIN_LOOP_INTERVAL=500 \
    INDEX_ADDRESSES=true \
    INDEX_BITCOIN_TRANSACTIONS=true \
    INDEX_SPENT_OUTPUTS=true \
    ENABLE_TCP_SUBSCRIPTIONS=true

# Arch Network deployments configure the HTTP and TCP listener assignments at runtime.
EXPOSE 3030 8080

# Expose the mountpoint for the data dir
VOLUME ["/home/titan/data"]

# Copy entrypoint script and set it as the container entrypoint
COPY --chown=root:root entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]

CMD ["titan"]