# GPU preview profiling (RT0)

The CPU renderer remains the pixel reference. Profiling does not change shaders or precision.
The `profile` Cargo feature on `effectcraft-cli` enables bounded CPU spans and optional GPU
timestamps. Ordinary builds compile CPU spans to no-ops and request no timestamp features.

On a native machine with a GPU, run the complete suite with one command:

```sh
CARGO_BUILD_JOBS=4 nice -n 10 cargo xtask bench --gpu
```

Pass --bin with an existing profiling CLI to skip compilation. Use the lane's existing `CARGO_TARGET_DIR`. This is a development benchmark build, not an app or
release build. Record its profile, commit, adapter and system load with the results. `--quick`
uses eight frames for a smoke check; the default uses 60. `--scenario mograph-1080`,
`--scenario footage-4k` or `--scenario adv3d-1080` selects one workload. A supplied `--project`
and `--comp` use an existing project. `--frames` is bounded to 1–600.

The generated suite contains:

- `footage-4k`: separate H.264 4:2:0 and ProRes 4:2:2 10-bit clips, generated locally by external
  ffmpeg, with Levels, Curves, Gaussian Blur, Glow, Tint, Exposure, Sharpen and a feathered mask.
  ffmpeg is a fixture generator only, never linked or shipped. Generated files stay in `target`.
- `mograph-1080`: 80 original shape/text layers with animated transforms, trim paths, masks
  and drop-shadow styles. No Motion Library or third-party artwork is required.
- `adv3d-1080`: the CLI's Advanced 3D scene with a camera move, depth of field and motion blur.

`target/bench/gpu/results.json` contains completed viewer-frame p50/p95, a separately reported
cold frame, upload/readback traffic and CPU/GPU stage totals for Full and Half. Each workload
also has its own JSON and Chrome trace file. GPU initialization/pipeline compilation occurs
before these frame measurements and is reported separately as `initializationMs`. `gpuDiagnosticWindowMs` includes the gaps between the first and last recorded GPU passes, unlike the sum of kernel durations. Consecutive times use exact frame ticks; this is not a
benchmark of repeatedly fetching one finished frame.

Latency samples are collected with trace recording stopped. One additional moving frame is
traced separately to avoid folding instrumentation overhead into the sample distribution.
The CPU spans are **inclusive**, so nested spans overlap. GPU durations are adapter timestamps,
on their own trace lane with its origin normalized independently, **not correlated to the CPU clock**. Compare aggregate stage
durations; the trace does not establish CPU/GPU overlap. Headless display preparation plus a
completion wait is measured; actual window presentation and display latency are not measured.

Timestamp queries are requested only when the adapter supports them. Existing shared devices
without the feature still render, with CPU timing and an explicit unavailable indication. The
recorder retains at most 100,000 events. At most eight query batches (1024 queries each) may be
in flight. Failures/capacity losses are counted in `droppedTraceEvents`; do not treat an incomplete
trace as a complete kernel breakdown. Async results from earlier recordings are discarded.

The benchmark refuses missing GPUs and failed GPU viewer frames rather than silently measuring
CPU fallback. CPU-only layer work and unsupported individual effects inside the compositor are
still allowed and visible in CPU spans. Use the existing CPU/GPU oracle suite to check fidelity;
timings alone do not establish correctness or a performance improvement.
