title: perf(media): open movies through a file reader and probe from headers
branch: upstream/perf-movie-reader
depends: none

## What
Movies open through a file reader: the sample index is read when the file opens, and samples are read on demand. Audio and movie probes read the headers instead of reading and decoding the whole file.

Standalone audio is probed with `symphonia` (MPL-2.0; the same build FilmCraft decodes with, already in `Cargo.lock`), now a direct workspace dependency of `effectcraft-media`.

## Why
Opening or probing a long movie read the entire file into memory first.

## How tested
- `header_probe_matches_a_whole_file_probe`: the header probes of FLAC, MP3 (with and without a Xing tag), H.264 MP4, VP9 WebM, WAV and PNG fixtures equal probing the whole file in memory.
- On this branch on top of `main`: `cargo clippy -p effectcraft-media --all-targets -- -D warnings` and `cargo test -p effectcraft-media` pass.

Ported from the fork. The second commit drops an AVIF sniff that only the fork has (no `avif` module here).
