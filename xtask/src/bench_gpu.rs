//! Real-time GPU preview suite; generated media stays under target/bench.
use super::*;

fn mograph() -> Cmds {
    let mut c = vec![comp("mograph-1080", 1920, 1080, 30.0)];
    for i in 0..80 {
        if i % 2 == 0 {
            c.push(("layer.newShape", json!({"kind": "rect", "size": [150, 110], "fill": hex(i), "stroke": "#ffffff", "strokeWidth": 3})));
            if i % 10 == 0 {
                c.push(("layer.addShapeItem", json!({"kind": "trim"})));
                c.push(key("contents/trim/end", 0.0, json!(30)));
                c.push(key("contents/trim/end", 10.0, json!(100)));
            }
        } else {
            c.push(("layer.newText", json!({"text": format!("Motion {i}"), "size": 48, "fill": hex(i)})));
        }
        moving(&mut c, i, 1920.0, 1080.0, 10.0);
        if i % 16 == 0 {
            c.push(("layer.style.add", json!({"style": "dropShadow"})));
        }
        if i % 8 == 0 {
            c.push(("mask.new", json!({"vertices": [[-100,-80],[100,-80],[100,80],[-100,80]], "closed": true, "mode": "Add"})));
        }
    }
    c
}

fn footage(clip: &Path) -> Cmds {
    let mut c = vec![
        comp("footage-4k", 3840, 2160, 30.0),
        ("file.import", json!({"paths": [clip.to_string_lossy()], "addToComp": true})),
        ("layer.select", json!({"layers": ["#1"]})),
    ];
    for (id, property, value) in [
        ("ec.color.levels", "gamma", json!(1.2)),
        ("ec.color.curves", "rgb", json!("0,0 0.5,0.6 1,1")),
        ("ec.blur.gaussian", "blurriness", json!(12)),
        ("ec.stylize.glow", "radius", json!(20)),
        ("ec.color.tint", "amount", json!(25)),
        ("ec.color.exposure", "master/exposure", json!(0.4)),
        ("ec.blur.sharpen", "amount", json!(15)),
    ] {
        c.push(effect(id));
        // The most recently applied effect is selected by its index in the stack.
        let index = c.iter().filter(|(id, _)| *id == "effect.apply").count();
        c.push(("prop.set", json!({"path": format!("effects/#{index}/{property}"), "value": value})));
    }
    c.push(("mask.new", json!({"vertices": [[120,120],[3720,120],[3720,2040],[120,2040]], "closed": true})));
    c.push(set("masks/#1/feather", json!([100, 100])));
    c
}

pub(super) fn run(root: &Path, o: &Opts) -> Result<(), String> {
    let candidate = match &o.bin {
        Some(bin) => bin.clone(),
        None => {
            eprintln!("bench: building the profiling CLI in the current Cargo target directory");
            let status = Command::new(env!("CARGO"))
                .current_dir(root)
                .env("CARGO_BUILD_JOBS", "4")
                .args(["build", "-p", "effectcraft-cli", "--features", "profile"])
                .status()
                .map_err(|e| e.to_string())?;
            if !status.success() {
                return Err(format!("profiling CLI build failed: {status}"));
            }
            let output =
                Command::new(env!("CARGO")).current_dir(root).args(["metadata", "--no-deps", "--format-version", "1"]).output().map_err(|e| e.to_string())?;
            if !output.status.success() {
                return Err("cannot locate Cargo target directory".into());
            }
            let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
            let target = metadata.get("target_directory").and_then(Value::as_str).ok_or("Cargo returned no target directory")?;
            PathBuf::from(target).join("debug").join(format!("effectcraft-cli{}", std::env::consts::EXE_SUFFIX))
        }
    };
    let bin = std::fs::canonicalize(&candidate).map_err(|e| format!("{}: {e}", candidate.display()))?;
    let dir = root.join("target/bench/gpu");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if !o.start.is_finite() || o.start < 0.0 {
        return Err("--start must be finite and nonnegative".into());
    }
    let frames = o.frames.unwrap_or(if o.quick { 8 } else { 60 });
    if frames > 600 {
        return Err("--gpu --frames must be in 1..=600".into());
    }
    let scenarios = o.scenarios.clone().unwrap_or_else(|| vec!["footage-4k".into(), "mograph-1080".into(), "adv3d-1080".into()]);
    let context = RunContext::new(root, &bin, o);
    let mut reports = Vec::new();
    if scenarios.iter().any(|s| !["footage-4k", "mograph-1080", "adv3d-1080"].contains(&s.as_str())) {
        return Err("unknown GPU scenario; use footage-4k, mograph-1080 or adv3d-1080".into());
    }
    for name in scenarios {
        let variants: &[&str] = if name == "footage-4k" { &["h264", "prores"] } else { &[""] };
        for codec in variants {
            let label = if o.project.is_some() {
                "custom".into()
            } else if codec.is_empty() {
                name.clone()
            } else {
                format!("{name}-{codec}")
            };
            let project = dir.join(format!("{label}.ecproj"));
            let mut cmd = Command::new(&bin);
            cmd.args(["bench", "--gpu", "--rt-profile", "--n", &frames.to_string(), "--time", &o.start.to_string()]);
            if let (Some(project), Some(comp)) = (&o.project, &o.comp) {
                cmd.arg("--project").arg(project).arg("--comp").arg(comp);
            } else {
                match name.as_str() {
                    "mograph-1080" => {
                        build_project(&bin, &mograph(), &project)?;
                        cmd.arg("--project").arg(&project).args(["--comp", "mograph-1080"]);
                    }
                    "adv3d-1080" => {
                        cmd.arg("--adv3d");
                    }
                    "footage-4k" => {
                        let seconds = (o.start + (f64::from(frames) + 2.0) / 30.0).ceil();
                        if seconds > 30.0 {
                            return Err("generated footage benchmark exceeds its 30-second comp".into());
                        }
                        let extension = if *codec == "h264" { "mp4" } else { "mov" };
                        let clip = dir.join(format!("generated-{seconds:.0}s.{extension}"));
                        let partial = dir.join(format!("generated-{seconds:.0}s.partial.{extension}"));
                        if o.regen || !clip.exists() {
                            // External fixture generator only. Neither ffmpeg nor its code is shipped.
                            let mut generator = Command::new("ffmpeg");
                            generator.args([
                                "-v",
                                "error",
                                "-y",
                                "-f",
                                "lavfi",
                                "-i",
                                "testsrc2=size=3840x2160:rate=30",
                                "-t",
                                &format!("{seconds:.0}"),
                                "-threads",
                                "4",
                            ]);
                            if *codec == "h264" {
                                generator.args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"]);
                            } else {
                                generator.args(["-c:v", "prores_ks", "-profile:v", "0", "-pix_fmt", "yuv422p10le"]);
                            }
                            let status = generator.arg(&partial).status().map_err(|e| format!("fixture generator ffmpeg: {e}"))?;
                            if !status.success() {
                                return Err(format!("fixture generator failed: {status}"));
                            }
                            std::fs::rename(&partial, &clip).map_err(|e| format!("publishing generated fixture: {e}"))?;
                        }
                        build_project(&bin, &footage(&clip), &project)?;
                        cmd.arg("--project").arg(&project).args(["--comp", "footage-4k"]);
                    }
                    _ => return Err(format!("unknown GPU scenario {name}")),
                }
            }
            cmd.arg("--trace").arg(dir.join(format!("{label}-trace.json")));
            eprintln!("bench: GPU {label}");
            let load_before = loadavg();
            let out = cmd.output().map_err(|e| e.to_string())?;
            eprint!("{}", String::from_utf8_lossy(&out.stderr));
            if !out.status.success() {
                return Err(format!("GPU benchmark failed: {}", out.status));
            }
            let report: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("GPU benchmark JSON: {e}"))?;
            std::fs::write(dir.join(format!("{label}.json")), &out.stdout).map_err(|e| e.to_string())?;
            reports.push(json!({"scenario": label, "report": report, "loadBefore": load_before, "loadAfter": loadavg()}));
            if o.project.is_some() {
                break;
            }
        }
        if o.project.is_some() {
            break;
        }
    }
    let result = json!({"suite": "RT0", "reports": reports, "runId": context.run_id, "git": context.git, "dirty": context.dirty, "bin": context.bin, "cores": context.cores, "quick": context.quick});
    append_line(&dir.join("history.jsonl"), &result)?;
    std::fs::write(dir.join("results.json"), serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("GPU results and traces: {}", dir.display());
    Ok(())
}
