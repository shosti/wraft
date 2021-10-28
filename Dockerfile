FROM rust:1.56.0-bullseye AS builder
COPY webrtc-introducer /build/webrtc-introducer
COPY webrtc-introducer-types /build/webrtc-introducer-types
RUN cd /build/webrtc-introducer && cargo build --release

FROM debian:bullseye
COPY --from=builder /build/webrtc-introducer/target/release/webrtc-introducer /app/webrtc-introducer
USER 1000:1000
CMD /app/webrtc-introducer 0.0.0.0:$PORT
