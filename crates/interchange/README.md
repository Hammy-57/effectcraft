# Timeline interchange

This crate converts FilmCraft's timeline interchange model to/from editable EffectCraft compositions.
The format implementations come from the first-party FilmCraft crates pinned in the workspace.

## FilmCraft composition links

New FilmCraft OTIO writers put `keyframe_time_base: "source"` in
`metadata.filmcraft`. For these documents, Motion, Opacity and audio parameter keyframes are already
in source-media time, which is EffectCraft layer time. Import must not apply source trims/stretch to
those key times again. Untagged legacy files retain their clip-local time conversion.

`otio_source_time_keys_do_not_apply_trim_and_speed_twice` checks tagged source-time keys and untagged
legacy behavior; `imports_fcp7_xml_fcpxml_and_otio` checks the existing format paths.

The engine's `link.capabilities` command advertises protocol 1, numeric composition IDs and
source-time keyframe support. `file.importTimeline` reports `supportsSourceTimeKeys: true` alongside
its import result. FilmCraft uses these signals to refuse incompatible renderers safely.
