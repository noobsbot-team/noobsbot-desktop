# Performance log (plan.md Part I)

## M1 — 2026-10-17

Nested (winit on X11), Intel RPL-U iris, Mesa 26.2.4, release build, one noobsbot window open.

| Target | Budget | Measured | |
|---|---|---|---|
| RAM | < 60 MB | **37 MB** USS (PSS 79 MB, RSS 177 MB) | pass |
| CPU idle | 0% | **0 wakeups** in 10 s with no client animating; 0.3% while the terminal blinks its cursor (a full-damage frame every 530 ms, which we must draw) | pass |
| Frame time | | 1.1 ms average, 4.5 ms worst while typing; no missed deadlines | |
| Startup | < 500 ms | 85–120 ms from launch to first frame | pass |
| Binary | small | 9.6 MB stripped | |

**RAM is USS** (private pages: what this process alone costs). Plain RSS also counts Mesa's shared code — libLLVM
(80 MB) and libgallium (47 MB) — which is 97% shared clean pages mapped by every GL process on the system, so RSS
overstates the compositor by ~140 MB. Mesa's private share is ~3 MB and is included in the USS figure.

How:
- RAM: `Private_Clean + Private_Dirty` from `/proc/<pid>/smaps_rollup`.
- Idle: per-thread CPU ticks and voluntary context switches from `/proc/<pid>/task/*/` over 10 s.
- Frame time: `RUST_LOG=noobsbot_desktop=trace`, the `frame render_time=` lines while typing.
- Startup: first log line to the first `frame render_time=` line.

Robustness (E1.11): `scripts/fuzz-client.py` ran 60–90 s per seed (seeds 1–4, ~130 000 connections each) against the
debug build without a panic, after vendoring a fix for a `wayland-backend` 0.3.17 panic it found
(`wl_registry.bind` with a null interface).

Glass stress test (E1.4): `--test-scene` at 1920×1080 rebuilds all 12 material cards (6 materials × light/dark,
elevations 1–5) every frame, repainting ~55% of the screen. 2.5 ms per frame CPU+GPU (0.66 ms CPU); without shadows
1.9 ms, so shadows cost ~0.6 ms. The GPU share is measured by waiting on the frame's fence, which also counts
submission latency and the iGPU clocking up — GL timer queries (M6) give the real GPU time. **The < 1 ms GPU budget is
not shown to hold under this stress test**; normal frames repaint only what changed.

The plan's 9-slice shadow cache was built and measured, then reverted: it matched the per-pixel shadows (PSNR 58.7 dB)
but made the stress frame slower — 3.2 ms (CPU 1.06 ms) instead of 2.4 ms (CPU 0.66 ms). The slices cover fewer
pixels than the single quad, so on this iGPU the shadow cost is per draw call, not per pixel. Next lever: GL timer
queries (M6) to see where GPU time actually goes; re-test slicing at 4K there.

Not measured yet: input latency (needs a real display), GPU memory, heaptrack run.
