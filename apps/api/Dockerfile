FROM rust:1.99.1-trixie@sha256:8a4e90c7946b74626488b1446f712c0fe0ed1a596cbd3d871684bc9c5433ae07 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY apps/api/Cargo.toml apps/api/build.rs apps/api/
COPY apps/api/migrations/ apps/api/migrations/
COPY apps/api/src/ apps/api/src/
RUN cargo build --locked --release -p tikkit-api --bin tikkit-api

FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 tikkit \
    && useradd --uid 10001 --gid 10001 --no-create-home tikkit \
    && install -d -m 0700 -o tikkit -g tikkit /var/lib/tikkit/screenshots
COPY --from=build /src/target/release/tikkit-api /usr/local/bin/tikkit-api
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/tikkit-api"]
