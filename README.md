# WRaft: Raft in WebAssembly

## What is this?

A toy implementation of the [Raft Consensus Algorithm](https://raft.github.io/)
for [WebAssembly](https://webassembly.org/), written in Rust. Basically, it
synchronizes state across three browser windows in a peer-to-peer fashion.

## Why is this?

Because I was curious to see if I could get it to work 😄. I can't think of any
real-world use-cases off the top of my head, but if you can, please open an
issue and let me know!

## How does it work?

WRaft uses [WebRTC data
channels](https://webrtc.org/getting-started/data-channels) to set up
communication between the browser windows. Sadly [WebRTC isn't purely
peer-to-peer](https://www.youtube.com/watch?v=Y1mx7cx6ckI), so there's a
separate WebSocket-based service (`webrtc-introducer`) that "introduces" the
browser windows to each other before the cluster can start. The browser windows
can be on the same computer or different machines on a LAN (or different
networks, theoretically, but I haven't tested that yet). Firefox and Chrome (or
any combination of the two) seem to work; Safari seems to not work.

Once the browser windows have been introduced to each other, a Raft cluster is
started and messages sent to one browser window are reliably replicated to all
three in a consistent order (as long as everything's working correctly 😉). The
messages are persisted to the browser's local storage so they'll survive browser
restarts. The cluster will continue functioning normally even if one window
stops, and can recover if two windows stop.

The "replicated messages" could be any
[serializable](https://docs.serde.rs/serde/trait.Serialize.html) Rust type.
There's a `raft::State` trait that allows the user to "plug in" any
state-machine-like-thing into the Raft server (using Rust generics). For the
example apps, I used [yew](https://yew.rs/) with the "state machine" more or
less mapping to the application state. (You could also use a `HashMap` to get a
distributed key/value store à la etcd.)

There's currently no way to expand beyond three browser windows (or any notion
of a "client" outside of the cluster servers). I have some ideas for how it
might work, though.

## Can I try it?

Yes! There are two basic "demo apps" included in the library, publicly hosted at
https://wraft0.eevans.co/. The apps are:

- [Synchronized TodoMVC](https://wraft0.eevans.co/todo).
- An extremely basic [benchmarking tool](https://wraft0.eevans.co/bench) to get
  a sense of performance.

To use either app, you'll need to open the app in different browser windows with
different hosts (`wraft0.eevans.co`, `wraft1.eevans.co`, and `wraft2.eevans.co`)
(they need to be different host names so they have independent Local
Storage). The app should then start up and you can try it!

## Is it fast?

I don't have much to compare it to, but from some basic testing it seems pretty
fast to me! In the best-case scenario (three Chromium browsers on the same
machine) I've seen ~2000 writes/second which should be enough for any use case I
can think of 😄. (The bigger issue is that you hit the local storage quota
pretty fast; log compaction would have to be implemented to work around that.)
Firefox sadly seems to top out at ~400 writes/second for reasons I haven't dug
into yet.

## What parts of Raft are implemented?

For the Raft nerds out there, so far I've only implemented the "basic algorithm"
(basically what's in the [TLA spec](https://github.com/ongardie/raft.tla)). To
make it more useful, you'd probably need:

- Log compaction/snapshots (the API is designed to make this possible, but it's
  completely unimplemented).

- Cluster membership changes (I haven't really looked into this yet).

No promises that either of those will ever happen, but I might try implementing
them if I have time.

## Should I use it in production?

**No!** (At least not in its current state.) Documentation, error handling, and
testing are basically non-existent, and I haven't implemented some harder parts
of Raft like log compaction and cluster membership changes. There are a few bugs
I know about and almost certainly many more I don't!

If you have an actual use case for this, open an issue to let me know and I'll
think about turning it into a proper Crate and/or NPM package.
