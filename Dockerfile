FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin xmrgate

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home /data xmrgate \
 && mkdir /data && chown xmrgate /data
COPY --from=build /src/target/release/xmrgate /usr/local/bin/xmrgate
USER xmrgate
ENV XMRGATE_LISTEN=0.0.0.0:3080 XMRGATE_DATA=/data
VOLUME /data
EXPOSE 3080
CMD ["xmrgate"]
