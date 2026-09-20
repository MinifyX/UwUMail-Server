# syntax=docker/dockerfile:1.7
#
# Builds UwUMail Server from source. CI uses docker/Dockerfile.release with
# binaries it cross-compiled before, which is much faster for arm64.

FROM node:24-slim AS web
WORKDIR /web
RUN corepack enable
COPY web/package.json web/pnpm-lock.yaml ./
RUN --mount=type=cache,target=/root/.local/share/pnpm/store pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

# The webmail comes from its own repository, at the commit webmail.pin names. A full hash, never a
# branch: the same server tag has to bake in the same webmail whenever it is built.
FROM node:24-slim AS webmail
WORKDIR /src
RUN corepack enable \
 && apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git \
 && rm -rf /var/lib/apt/lists/*
COPY webmail.pin ./
RUN repo="$(sed -n 's/^repo=//p' webmail.pin)" \
 && commit="$(sed -n 's/^commit=//p' webmail.pin)" \
 && git init -q webmail \
 && git -C webmail remote add origin "$repo" \
 && git -C webmail fetch -q --depth 1 origin "$commit" \
 && git -C webmail checkout -q FETCH_HEAD \
 && test "$(git -C webmail rev-parse HEAD)" = "$commit"
WORKDIR /src/webmail
RUN --mount=type=cache,target=/root/.local/share/pnpm/store pnpm install --frozen-lockfile
RUN pnpm build

FROM rust:1-slim-trixie AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake clang \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# The web app is embedded into the binary (crates/uwumail-web/build.rs), and so is the webmail.
COPY --from=web /web/dist /src/web/dist
COPY --from=webmail /src/webmail/dist /src/webmail/dist
# The cached target directory can hold artifacts from an earlier build whose sources look
# unchanged to cargo (context mtimes are unreliable on Docker Desktop), which then embeds stale
# web assets or misses new code. Touching the workspace sources forces those crates to rebuild;
# the registry cache of third-party crates stays.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    touch crates/*/src/lib.rs crates/*/src/main.rs crates/*/build.rs 2>/dev/null; \
    cargo build --release --locked -p uwumail-server \
 && cp target/release/uwumail-server /usr/local/bin/uwumail-server

# Prepare the binary on Debian, which still has the tools distroless does not: set the file
# capability that lets the unprivileged user bind the low mail and web ports, and create /data
# owned by the runtime user.
FROM debian:trixie-slim AS prep
RUN apt-get update \
 && apt-get install -y --no-install-recommends libcap2-bin \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/uwumail-server /usr/local/bin/uwumail-server
RUN chmod 755 /usr/local/bin/uwumail-server \
 && setcap cap_net_bind_service=+ep /usr/local/bin/uwumail-server \
 && install -d -o 10001 -g 10001 /data

# The runtime is distroless: glibc, ca-certificates and tzdata, but no shell and no package
# manager, so a break-in has almost nothing to build on. Pinned by digest; cc-debian13 is trixie,
# so its glibc matches the binary. The file capability survives the plain COPY (a --chown would
# clear it) and lets the unprivileged user bind the low ports; the compose files keep only
# NET_BIND_SERVICE for it and drop every other capability.
FROM gcr.io/distroless/cc-debian13@sha256:4594d59540d1948417f6ca2829ddd9294493a7c68b7528f4dd459de7f203a750
COPY --from=prep /usr/local/bin/uwumail-server /usr/local/bin/uwumail-server
COPY --from=prep --chown=10001:10001 /data /data
USER 10001:10001
# The data directory is the one writable place, also for HOME, so nothing needs to write to the
# read-only root file system.
ENV UWUMAIL_DATA_DIR=/data HOME=/data
VOLUME /data
EXPOSE 25 80 443 465 587 993
HEALTHCHECK --interval=30s --timeout=10s --start-period=20s CMD ["/usr/local/bin/uwumail-server", "health"]
ENTRYPOINT ["/usr/local/bin/uwumail-server"]
CMD ["serve"]
