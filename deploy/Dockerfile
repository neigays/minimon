# minimon server 镜像。构建上下文为仓库根目录：
#   docker build -f deploy/Dockerfile -t minimon-server .

# ---- 前端 ----
FROM node:24-alpine AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

# ---- server（musl 静态链接）----
FROM rust:1.100-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY --from=web /src/web/dist web/dist
RUN cargo build --release --locked -p minimon-server \
    && cp target/release/minimon-server /minimon-server

# ---- 运行 ----
FROM alpine:3
RUN addgroup -S minimon && adduser -S -G minimon -H minimon \
    && mkdir /data && chown minimon:minimon /data
COPY --from=build /minimon-server /usr/local/bin/minimon-server
USER minimon
ENV MINIMON_LISTEN=0.0.0.0:25780 \
    MINIMON_DATA_DIR=/data
VOLUME /data
EXPOSE 25780
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s \
    CMD wget -qO /dev/null http://127.0.0.1:25780/api/health || exit 1
ENTRYPOINT ["minimon-server"]
CMD ["serve"]
