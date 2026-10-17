# One-command cloud preview (CR10)

`scripts/fork/cloud.sh` connects the Mac app to a vast.ai GPU machine **Elias rented himself**.
Fork-only (keep it out of upstream pull requests). It never rents, stops or destroys an instance:
destroying stays manual in the vast.ai console.

```bash
scripts/fork/cloud.sh connect root@<ip> -p <port>   # everything below, in one go
scripts/fork/cloud.sh status                        # tunnel, server commit, GPU, free VRAM/disk, pending uploads
scripts/fork/cloud.sh disconnect                    # app back to This Mac, tunnel closed, server process stopped
```

Open the project in the Mac app first (the mirror reads the project from the app on control port 9877),
or pass `--project "/path/10-10 Edit.ecproj"`.

## What `connect` does

1. **Machine check.** GPU, free VRAM, free disk, `/workspace/effectcraft` and `cargo` present, plus the
   remote-gpu skill's `check.sh` (informational).
2. **Matching `effectcraft-cli`.** The Mac's `main` commit is compared with the commit the machine's
   `target/local/effectcraft-cli` was built from (`target/local/.built-commit`). If different, only the
   missing git commits are pushed over ssh (a few MB, not GitHub, no binary), the checkout is
   fast-forwarded, and `cargo build --profile local -p effectcraft-cli` runs there in the background
   (`nice`, CPUs 100-111, `EC_BUILD_CPUS` to change). Progress is printed; log: `/workspace/cloud-build.log`.
   If the machine's checkout can't fast-forward (local changes, other branch) it stops and says so.
3. **Mirror (CR7).** `effectcraft-cli mirror`: files up to 100 MB are sent once and deduplicated by hash,
   fonts too; files over 100 MB are never sent from the Mac. If one has no Google Drive link the run
   stops and lists it. Paste the link in `<project>.cloud.json` (`"driveLinks": {"file name": "link"}`) or
   run `cloud.driveLinks {"set": {"file name": "link"}}` in the app, then run `connect` again. Ends with
   `cloud.preflight` on the machine.
   **Motion Library cards** (files under the Mac's library cache, `…/Motion Library/blobs/<sha256>/<name>`)
   are never uploaded: `effectcraft-cli library-pull` on the machine fetches each blob from the library's
   Worker (R2) with the read token, checks sha256 + size and puts it at the same path. If that fails (no
   token there, Worker down, an old `effectcraft-cli`) the mirror says so and uploads those files from the
   Mac instead (still only up to 100 MB). The JSON result carries `fromLibrary` / `libraryBytes`.
4. **Frame server.** `effectcraft-cli serve --gpu --frames <port>` under `nohup` (also serves the CR6a
   H.264 playback stream), `EFFECTCRAFT_FRAME_GPU_SLOTS=4`, the mirror's `cloud.env` (fonts, CJK locale).
   Skipped when it already runs at the right commit with the same token. Log:
   `/workspace/cloud-serve-<port>.log`.
5. **Tunnel + app.** `ssh -M -S … -L 127.0.0.1:<port>:127.0.0.1:<port>` (`ExitOnForwardFailure`,
   `ServerAliveInterval`), then the running app gets `previews.cloudAddress` and `previews.previewOn = cloud`
   through its control channel. If the app isn't running it prints what to set in
   Settings ▸ Previews ▸ Cloud Preview.

Running `connect` twice is safe: finished steps are skipped (build, mirror bytes, server, tunnel).

## Files and secrets

- Connection state (ip, ports, no secrets): `~/Library/Application Support/EffectCraft/cloud-connection.env`
  (so `status` and `disconnect` need no arguments).
- Frame token: `…/EffectCraft/cloud-frame-token` (mode 600, made on the first connect; the app reads it, CR5).
  It reaches the server as an environment variable of the serve process only (root on the machine can read
  it in /proc, which is fine on a machine you rent alone; it is passed once as an ssh argument).
- Motion Library read token: `connect` sends `…/EffectCraft/motion-library.read.json` (never the publish
  token `motion-library.json`; it refuses when both hold the same token) over ssh **stdin** into
  `~/.config/effectcraft/motion-library.json` for root (library-pull) and for `user` (the browser desktop
  app's Motion Library panel), mode 600. Not in a command line, a log or the vast template.
- A build needs about 12 GB free on /workspace: below that `connect` refuses and says what to free.
- The tunnel's control socket: `/tmp/ec-cloud-<uid>.sock`.
- `EFFECTCRAFT_CONFIG_DIR` moves all of the above (tests use it); ssh key `~/.ssh/id_ed25519`
  (`EC_CLOUD_KEY`); `EC_CLI` picks the Mac's `effectcraft-cli`.

## Options

`--frames-port N` (default 9878), `--local-port N`, `--app-port N` (default `$EFFECTCRAFT_CONTROL_PORT` or 9877),
`--project F.ecproj`, `--no-mirror`, and:

`--idle MIN` (off by default): a small watchdog on the machine stops the **serve process** (never the
instance, which keeps billing) after MIN minutes with no app connected. Re-run `connect` to restart it.

## Tested

3090, 17 Oct 2026: connect from a cold checkout (build, mirror, serve, tunnel), second connect (nothing
repeated), `status`, app copy on its own control port showing `connected / ready`, idle stop, `disconnect`.
