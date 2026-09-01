# DJ Server

A Rust/Axum web application for synchronizing two browser-based DJ decks across a shared room. It serves the frontend from `public/` and exposes a WebSocket endpoint at `/ws`.

File transfer between clients is not yet supported; each client needs to load the same audio files independently.

## Principles of operation

The sync model is inpsired by multiplayer game netcode, applied to a DJ mixer instead of a shooter:

- **Server-authoritative state.** The server is the sole source of truth for every deck's transport, cue point, loop, and the room's crossfader. Clients only ever send requests; the server decides what actually happened and broadcasts it to everyone, including the sender.
- **Client-side prediction.** A client dragging a continuous control (tempo, crossfader) applies its own input immediately, without waiting for a round trip, then reconciles as the server's broadcast echoes back - the same trade a FPS makes to keep your own movement feeling instant while everyone else's stays authoritative.
- **Revision numbers as sequence numbers.** Each deck (and the crossfader) carries a monotonically increasing revision counter. Clients discard any update at or behind the last one they applied, so an out-of-order or duplicate broadcast can't corrupt local state.
- **Entity interpolation for remote rendering.** A client watching someone *else* drag the tempo fader renders ~90ms behind the live edge, interpolating between the two samples straddling that delayed instant, instead of snapping to each one as it arrives - the same "render the past, smoothly" trick used to make other players' movement look continuous on a discrete update rate.
- **Scheduled, not instant, execution.** A discrete action like Play takes effect at a fixed lead time (150ms) in the future, not the instant the server receives it - enough slack for every client's network to receive and arm it before it fires, the same shape as a race game broadcasting a start time instead of a start signal so every client's flag drops together.

### Clocks

Three separate clocks are in play:

1. **Server clock** (`src/clock.rs`) - a single monotonic clock (`Instant`, not wall time) that every scheduling decision is expressed in terms of.
2. **Client wall clock** (`performance.now()`) - never assumed to agree with the server's. On connect, and periodically after, each client estimates its offset via an NTP-style round trip (timestamp out, server stamps its receipt and reply, timestamp back in), keeping whichever sample had the lowest round-trip time as the least-delayed measurement.
3. **Audio hardware clock** (`AudioContext.currentTime`) - governs when a browser's speakers actually produce sound, independent of the two clocks above. The client maps a target wall-clock instant onto it via the browser's output-timestamp correlation, and separately measures each browser's own fixed scheduling bias once per session with a silent probe, correcting for it directly rather than chasing it continuously.

## Build Requirements

- Rust 1.86.0 (see `rust-toolchain.toml`)

## Run locally

```bash
cargo run
```

Open <http://127.0.0.1:3000> in a browser.

To access it from another device on the LAN:

```bash
BIND_ADDRESS=0.0.0.0:3000 cargo run
```

For LAN browser audio APIs, use HTTPS:

```bash
BIND_ADDRESS=0.0.0.0:3000 TLS_ENABLED=true cargo run
```

The development certificate is generated and cached in `certs/`. Browsers will require accepting its warning. For production, terminate HTTPS in NGINX or another reverse proxy and leave `TLS_ENABLED=false`.

## Configuration

The server reads these environment variables:

| Variable | Default | Description |
| --- | --- | --- |
| `BIND_ADDRESS` | `127.0.0.1:3000` | Host and port to bind |
| `SCHEDULE_LEAD_TIME_MS` | `150` | Scheduling lead time in milliseconds; must be greater than zero |
| `TLS_ENABLED` | `false` | Enables auto-generated self-signed HTTPS |
| `TLS_CERT_DIR` | `certs` | Directory for the generated certificate and key |

## Test and build

```bash
cargo test --locked
cargo build --locked --release
```

The local release executable is `target/release/shared-audio-clock`. The deployment workflow builds a stripped, statically linked x86-64 musl executable at `target/x86_64-unknown-linux-musl/release/shared-audio-clock`. Deploy either executable together with the `public/` directory, since the server serves those files at runtime.
