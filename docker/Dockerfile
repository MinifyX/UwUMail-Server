# syntax=docker/dockerfile:1.7
#
# Builds UwUMail Server from source. CI uses docker/Dockerfile.release with
# binaries it cross-compiled before, which is much faster for arm64.

FROM rust:1-slim-trixie AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake clang \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p uwumail-server \
 && cp target/release/uwumail-server /usr/local/bin/uwumail-server

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tzdata libcap2-bin \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin uwumail \
 && mkdir -p /data \
 && chown uwumail:uwumail /data
COPY --from=build /usr/local/bin/uwumail-server /usr/local/bin/uwumail-server
# Mail and web ports are below 1024; this lets the unprivileged user bind them.
RUN setcap cap_net_bind_service=+ep /usr/local/bin/uwumail-server
USER uwumail
ENV UWUMAIL_DATA_DIR=/data
VOLUME /data
EXPOSE 25 80 443 465 587
HEALTHCHECK --interval=30s --timeout=10s --start-period=20s CMD ["uwumail-server", "health"]
ENTRYPOINT ["uwumail-server"]
CMD ["serve"]
