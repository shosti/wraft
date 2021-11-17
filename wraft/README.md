# WRaft: Raft in WebAssembly

This crate contains the Rust code for the WebAssembly module for WRaft. It has
a few modules:

- The top-level module (at `src/lib.rs`) contains sample web applications that
  use the Raft library

- The `src/webrtc_rpc` module contains code for making a makeshift "RPC
  framework" from WebRTC data channels (including code for introducing nodes to
  each other using a signaling server)

- The `src/ringbuf` module contains an extremely simple ring buffer
  implementation that's used by the `src/webrtc_rpc` module

- The `src/raft` module contains the actual Raft library code
