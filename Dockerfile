FROM rust:1.56.0-bullseye AS builder
COPY webrtc-introducer /build/webrtc-introducer
COPY webrtc-introducer-types /build/webrtc-introducer-types
RUN cd /build/webrtc-introducer && cargo build --release

FROM gcr.io/distroless/cc-debian11
COPY --from=builder /build/webrtc-introducer/target/release/webrtc-introducer /app
USER 1000:1000
CMD ["/app"]
