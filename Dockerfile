FROM rust:1-bookworm@sha256:adab7941580c74513aa3347f2d2a1f975498280743d29ec62978ba12e3540d3a AS builder
WORKDIR /workspace
COPY . .
RUN cargo build --locked --release -p ateliad

FROM debian:bookworm-slim@sha256:f9c6a2fd2ddbc23e336b6257a5245e31f996953ef06cd13a59fa0a1df2d5c252
RUN useradd --system --uid 10001 --create-home atelia
COPY --from=builder /workspace/target/release/ateliad /usr/local/bin/ateliad
USER atelia
ENTRYPOINT ["ateliad"]
