FROM rust:1-bookworm AS builder
WORKDIR /workspace
COPY . .
RUN cargo build --release -p ateliad

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --create-home atelia
COPY --from=builder /workspace/target/release/ateliad /usr/local/bin/ateliad
USER atelia
ENTRYPOINT ["ateliad"]
