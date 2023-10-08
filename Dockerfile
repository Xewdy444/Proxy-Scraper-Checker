FROM rust:slim as builder

WORKDIR /app

COPY . .

RUN apt-get update -y \
    && apt-get install -y \
    pkg-config \
    libssl-dev \
    && cargo build --release

FROM debian:stable-slim

RUN apt-get update -y \
    && apt-get install -y \
    libssl-dev \
    ca-certificates \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/proxy-scraper-checker /usr/local/bin/proxy-scraper-checker

ENTRYPOINT [ "/usr/local/bin/proxy-scraper-checker" ]
