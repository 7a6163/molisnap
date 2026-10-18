FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
ARG MOLI_VERSION=v1.1.17
ARG TARGETARCH
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl libfontconfig1 \
 && case "$TARGETARCH" in amd64) A=x86_64 ;; arm64) A=aarch64 ;; *) echo "unsupported arch $TARGETARCH" && exit 1 ;; esac \
 && curl -fsSL "https://github.com/lexmount/moli/releases/download/${MOLI_VERSION}/moli-${A}-unknown-linux-gnu.tar.gz" | tar xz -C /tmp \
 && mv /tmp/moli-*/moli /usr/local/bin/moli \
 # Moli's license terms require shipping its notices with the binary.
 && mkdir -p /usr/share/doc/moli \
 && mv /tmp/moli-*/LICENSE-* /tmp/moli-*/licenses /tmp/moli-*/license-metadata.json /usr/share/doc/moli/ \
 && rm -rf /tmp/moli-* \
 && apt-get purge -y curl && apt-get autoremove -y && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/molisnap /usr/local/bin/molisnap
ENV LISTEN_ADDR=0.0.0.0:3000 MOLI_BIN=/usr/local/bin/moli HOME=/tmp
USER nobody
EXPOSE 3000
CMD ["molisnap"]
