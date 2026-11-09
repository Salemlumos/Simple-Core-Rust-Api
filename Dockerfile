# syntax=docker/dockerfile:1.7
# Build multi-stage: compila em Debian trixie e roda em distroless (glibc),
# sem shell, como usuário não root. Não precisa de banco no build
# (SQLX_OFFLINE=true usa o diretório .sqlx versionado).

FROM rust:1.99-slim-trixie AS build
WORKDIR /src
ENV SQLX_OFFLINE=true CARGO_TERM_COLOR=never
ARG CARGO_FEATURES=""

# 1) Dependências: compiladas com fontes vazias para ficarem em cache
#    enquanto Cargo.toml/Cargo.lock não mudarem.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && touch src/lib.rs
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked ${CARGO_FEATURES:+--features $CARGO_FEATURES} \
 && rm -rf src

# 2) Código da aplicação (migrations são incorporadas ao binário).
COPY .sqlx ./.sqlx
COPY migrations ./migrations
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    touch src/main.rs src/lib.rs \
 && cargo build --release --locked ${CARGO_FEATURES:+--features $CARGO_FEATURES} \
 && cp target/release/api /api

FROM gcr.io/distroless/cc-debian13:nonroot
WORKDIR /app
COPY --from=build /api /app/api
USER nonroot:nonroot
EXPOSE 8080
ENTRYPOINT ["/app/api"]
CMD ["serve"]
