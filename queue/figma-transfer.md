title: feat(figma): Figma → EffectCraft transfer plug-in, bridge and file.importFigma
branch: upstream/figma-transfer
depends: none

## What
Figma → EffectCraft transfer: Figma frames, groups, shapes and text come in as native, editable
layers (shape layers with editable paths, rectangles/ellipses/polystars, solid and gradient fills,
strokes, text with style runs, frames as precomps, masks as track mattes, shadows as layer styles,
layer blur as Gaussian Blur). Node types without a native counterpart come in as renders placed
exactly over the node.

- `file.importFigma` (engine, `crates/engine/src/figma/`): node JSON from the plug-in or the Figma
  MCP (`payload` / `payloadPath`, no token), or a Figma link fetched through the REST API with a
  personal access token (`figma.setToken`, `FIGMA_TOKEN`). One undo step.
- `integrations/figma/plugin`: the EffectCraft Transfer plug-in for Figma.
- `effectcraft-cli figma-bridge`: the local HTTP endpoint the plug-in posts to (a plug-in UI can
  only make HTTP requests; the control channel is JSON lines), forwarding to the app's control
  channel.

This is the Figma part of #858, split out as asked.

## Security (from the review of #858)
1. **No token on redirects.** Requests that carry `X-Figma-Token` never pass `-L` (curl forwards
   custom headers to redirect targets; Figma's API doesn't redirect). Only the token-less,
   pre-signed image download follows redirects (at most 5). Every request is HTTPS-only
   (`--proto =https`, `--proto-redir =https`); plain HTTP is allowed only for a loopback
   `apiBase` (the tests' fake API). The options come from one function with a unit test.
2. **Secret files are created 0600.** `figma.setToken` and the bridge's pairing-code file are
   created with `OpenOptions` + mode 0600 on unix (an existing file with wider permissions is
   narrowed before the secret is written), instead of write-then-chmod.
3. **The pairing code is the real protection, so it's hardened.** Origin `null` is any sandboxed
   iframe, so: the code comes from the OS CSPRNG (`getrandom`, rejection-sampled into a
   32-letter alphabet, 12 characters, 60 bits); after 5 wrong codes in a row the bridge refuses
   codes for 60 s (429), a right code resets the count; request bodies are capped at 64 MB (413,
   documented in the README; large selections can be sent in parts); `--code` needs at least 8
   characters. Extra: the `Host` header must name the loopback address (no DNS rebinding).

curl stays for the optional REST path for now: the workspace has no HTTP/TLS stack, and model
downloads (`crates/engine/src/models.rs`) already go through the system curl the same way; the
token goes to curl on stdin (`-K -`), never on the command line. Happy to move both to a
pure-Rust client in a follow-up if you'd rather have one. The plug-in and MCP paths need no
token and no curl.

## How tested
- `cargo test -p effectcraft-engine --lib figma` (12: links, mapping, build, hostile payloads,
  the REST path against a loopback fake API, no `-L` with the token, 0600 token file).
- `cargo test -p effectcraft-cli --bin effectcraft-cli figma_bridge` (6: host/origin checks, code
  alphabet and length, wrong-code lockout and recovery, body cap, 0600 code file).
- `rustfmt --check` and `cargo check` on the affected crates.
- Not run here: the full `cargo xtask ci`, and the plug-in end to end in Figma.
