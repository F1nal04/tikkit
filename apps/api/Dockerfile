FROM rust:1.99.0-trixie@sha256:81015672d6939b0265f09adf96d5de125482bb1330106076956b0518e74edc90 AS build
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
