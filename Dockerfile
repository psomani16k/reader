FROM rust:1.92 as builder
WORKDIR /reader
COPY . .
RUN rustup target add x86_64-unknown-linux-musl
RUN cargo build --release --target x86_64-unknown-linux-musl

FROM alpine:3.14
COPY --from=builder /reader/target/x86_64-unknown-linux-musl/release/reader /usr/bin/reader
CMD ["reader"]
