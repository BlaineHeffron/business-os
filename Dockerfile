FROM node:22-bookworm-slim AS frontend
WORKDIR /source
COPY frontend/package.json frontend/package-lock.json frontend/
RUN npm --prefix frontend ci
COPY frontend frontend
RUN npm --prefix frontend run build

FROM rust:1.88-bookworm AS builder
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY apps apps
COPY --from=frontend /source/frontend/dist frontend/dist
RUN cargo build --locked --release -p bos-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /source/target/release/bos-server /usr/local/bin/bos-server
ENV BOS_STATE_DIR=/data
ENV BOS_SERVER_BIND=0.0.0.0:4400
EXPOSE 4400
VOLUME ["/data"]
ENTRYPOINT ["bos-server"]
