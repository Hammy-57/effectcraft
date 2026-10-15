//! Writes the seed corpus (`corpus/<target>/seed-*`): small valid inputs the fuzzer mutates from.
//! `cargo run --manifest-path fuzz/Cargo.toml --example seeds` (run by `cargo xtask fuzz --seeds`).

use std::path::Path;

use effectcraft_engine::demo::demo_project;

fn put(target: &str, name: &str, bytes: &[u8]) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus").join(target);
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join(format!("seed-{name}")), bytes)) {
        eprintln!("seeds: {target}/{name}: {e}");
    }
}

fn with_pick(pick: u8, rest: &[u8]) -> Vec<u8> {
    let mut v = vec![pick];
    v.extend_from_slice(rest);
    v
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let demo = demo_project();
    put("project_open", "demo.ecproj", demo.to_json_compact().as_bytes());
    put("project_open", "empty.ecproj", effectcraft_project::Project::default().to_json_compact().as_bytes());

    if let Some(cid) = demo.comps().map(|(id, _)| *id).next() {
        match effectcraft_lottie::export_comp(&demo, cid, &Default::default(), &|_| None) {
            Ok(r) => {
                put("lottie_import", "demo.json", r.json.to_string().as_bytes());
                put("lottie_import", "demo.lottie", &effectcraft_lottie::to_dotlottie(&r, "demo"));
            }
            Err(e) => eprintln!("seeds: lottie export: {e}"),
        }
    }

    for f in ["basic-shapes.svg", "paths-gradients.svg"] {
        if let Ok(b) = std::fs::read(root.join("crates/svg/tests/fixtures").join(f)) {
            put("svg_parse", f, &b);
            put("media_probe", f, &with_pick(13, &b));
        }
    }

    put("pdf_parse", "min.pdf", b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 4 0 R>>endobj\n4 0 obj<</Length 35>>stream\n1 0 0 rg 10 10 80 80 re f 0 0 1 RG\nendstream endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n");
    put(
        "pdf_parse",
        "min.eps",
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\nnewpath 10 10 moveto 90 10 lineto 50 90 lineto closepath 0 0 1 setrgbcolor fill\nshowpage\n",
    );

    put(
        "interchange_import",
        "cmx.edl",
        &with_pick(3, b"TITLE: Fuzz\nFCM: NON-DROP FRAME\n\n001  AX       V     C        00:00:00:00 00:00:02:00 01:00:00:00 01:00:02:00\n* FROM CLIP NAME: a.mov\n002  AX       AA/V  D    012 00:00:05:00 00:00:07:00 01:00:02:00 01:00:04:00\n"),
    );
    put(
        "interchange_import",
        "timeline.otio",
        &with_pick(2, br#"{"OTIO_SCHEMA":"Timeline.1","name":"T","tracks":{"OTIO_SCHEMA":"Stack.1","children":[{"OTIO_SCHEMA":"Track.1","kind":"Video","children":[{"OTIO_SCHEMA":"Clip.2","name":"a","source_range":{"OTIO_SCHEMA":"TimeRange.1","start_time":{"OTIO_SCHEMA":"RationalTime.1","value":0,"rate":24},"duration":{"OTIO_SCHEMA":"RationalTime.1","value":48,"rate":24}},"media_references":{"DEFAULT_MEDIA":{"OTIO_SCHEMA":"ExternalReference.1","target_url":"file:///a.mov"}},"active_media_reference_key":"DEFAULT_MEDIA"},{"OTIO_SCHEMA":"Gap.1","source_range":{"OTIO_SCHEMA":"TimeRange.1","start_time":{"OTIO_SCHEMA":"RationalTime.1","value":0,"rate":24},"duration":{"OTIO_SCHEMA":"RationalTime.1","value":12,"rate":24}}}]}]}}"#),
    );
    put(
        "interchange_import",
        "fcp7.xml",
        &with_pick(0, br#"<?xml version="1.0"?><xmeml version="5"><sequence><name>S</name><rate><timebase>24</timebase></rate><media><video><track><clipitem id="c1"><name>a</name><start>0</start><end>48</end><in>0</in><out>48</out><file id="f1"><name>a.mov</name><pathurl>file:///a.mov</pathurl></file></clipitem></track></video></media></sequence></xmeml>"#),
    );
    put(
        "interchange_import",
        "x.fcpxml",
        &with_pick(1, br#"<?xml version="1.0"?><fcpxml version="1.10"><resources><format id="r1" frameDuration="1/24s" width="1920" height="1080"/><asset id="r2" name="a" start="0s" duration="10s" hasVideo="1"><media-rep kind="original-media" src="file:///a.mov"/></asset></resources><library><event><project name="P"><sequence format="r1"><spine><asset-clip ref="r2" offset="0s" duration="2s" name="a"/></spine></sequence></project></event></library></fcpxml>"#),
    );

    for (i, e) in [
        "value",
        "wiggle(2, 30)",
        "loopOut(\"cycle\")",
        "[thisComp.width / 2, thisComp.height / 2]",
        "linear(time, 0, 1, 0, 100)",
        "var s = 0; for (var i = 0; i < 10; i++) { s += Math.sin(i); } s * 10",
        "thisLayer.transform.position.valueAtTime(time - 0.1)",
        "Math.max.apply(null, [1, 2, 3])",
        "posterizeTime(12); random(100)",
        "ease(time, 0, 2, [0, 0], [100, 50])",
    ]
    .iter()
    .enumerate()
    {
        put("expr_eval", &format!("{i}.js"), &with_pick(i as u8, e.as_bytes()));
    }

    let mut png = Vec::new();
    if let Ok(b) = std::fs::read(root.join("assets/app-icon/hicolor/16x16/apps/ai.storyteller.effectcraft.png")) {
        png = b;
    }
    if !png.is_empty() {
        put("media_probe", "icon.png", &with_pick(0, &png));
    }
    // A tiny 16-bit mono WAV (8 samples).
    let mut wav = Vec::new();
    let data: Vec<u8> = (0..8i16).flat_map(|i| (i * 1000).to_le_bytes()).collect();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&48_000u32.to_le_bytes());
    wav.extend_from_slice(&96_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    put("media_probe", "tone.wav", &with_pick(10, &wav));
    put("psd_parse", "header-only.psd", b"8BPS\x00\x01\x00\x00\x00\x00\x00\x00\x00\x03\x00\x00\x00\x02\x00\x00\x00\x02\x00\x08\x00\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xff\xff\xff\xff\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00");

    // Commands: (two little-endian index bytes, params). The index picks among the safe commands (see lib.rs); these
    // only seed the shape of a line, the fuzzer finds the rest.
    put(
        "engine_command",
        "lines",
        b"\x00\x00{}\n\x10\x00{\"layers\":[\"#1\"]}\n\x20\x00{\"path\":\"transform/position\",\"value\":[1e308,-1e308,0]}\n\x30\x00{\"time\":-5}\n\x40\x00{\"name\":\"\xc3\xa9\",\"frameRate\":0.0001}\n\x50\x00{\"effect\":\"Gaussian Blur\"}\n",
    );
    eprintln!("seeds: written to {}", Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus").display());
}
