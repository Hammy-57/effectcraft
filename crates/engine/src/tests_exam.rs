//! `exam.check` and the agent exam's checker ([`crate::exam`]): ops, JSON Pointers and `select`,
//! shorthand expansion, refusing non-queries, malformed checks, scores, and every task of
//! `evals/agent/tasks` solved by its reference solution.

use serde_json::{Value, json};

use crate::Session;
use crate::exam::{self, Check};

/// A 1920×1080 comp "Main" (5 s at 25 fps) with a 200×100 solid "Logo" and a null "Ctrl".
fn project() -> Session {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Main", "width": 1920, "height": 1080, "frameRate": 25, "duration": 5})).unwrap();
    s.execute("layer.newSolid", json!({"name": "Logo", "width": 200, "height": 100, "color": "#ff0000"})).unwrap();
    s.execute("layer.newNull", json!({"name": "Ctrl"})).unwrap();
    s
}

fn run(s: &mut Session, checks: Value) -> Value {
    s.execute("exam.check", json!({"checks": checks})).unwrap()
}

fn one(s: &mut Session, check: Value) -> Value {
    let r = run(s, json!([check]));
    r["results"][0].clone()
}

/// Evaluate one expectation on a value.
fn exp(v: &Value, e: Value) -> exam::Outcome {
    exam::expectation(v, &e, 0)
}

fn passes(v: &Value, e: Value) -> bool {
    let o = exp(v, e.clone());
    assert!(!o.malformed, "{e} is malformed: {}", o.reason);
    o.pass
}

#[test]
fn ops() {
    let v = json!({"n": 3, "f": 2.5, "s": "Hello World", "a": [1, 2, 3], "o": {"k": 1}, "nul": null, "pos": [960.0004, 540, 0],
        "layers": [{"name": "Logo", "kind": "solid"}, {"name": "Title", "kind": "text"}]});
    // eq / ne: numbers by value.
    assert!(passes(&v, json!({"path": "/n", "op": "eq", "value": 3.0})));
    assert!(passes(&v, json!({"path": "/a", "op": "eq", "value": [1.0, 2, 3]})));
    assert!(!passes(&v, json!({"path": "/s", "op": "eq", "value": "hello world"})));
    assert!(passes(&v, json!({"path": "/n", "op": "ne", "value": 4})));
    // approx: default tolerance, arrays (an expected prefix).
    assert!(passes(&v, json!({"path": "/pos", "op": "approx", "value": [960, 540]})));
    assert!(!passes(&v, json!({"path": "/pos", "op": "approx", "value": [961, 540]})));
    assert!(passes(&v, json!({"path": "/pos", "op": "approx", "value": [961, 540], "tol": 1.5})));
    assert!(passes(&v, json!({"path": "/f", "op": "approx", "value": 2.6, "tol": 0.2})));
    // compares.
    assert!(passes(&v, json!({"path": "/n", "op": "gt", "value": 2})));
    assert!(!passes(&v, json!({"path": "/n", "op": "gt", "value": 3})));
    assert!(passes(&v, json!({"path": "/n", "op": "gte", "value": 3})));
    assert!(passes(&v, json!({"path": "/f", "op": "lt", "value": 3})));
    assert!(passes(&v, json!({"path": "/f", "op": "lte", "value": 2.5})));
    assert!(!passes(&v, json!({"path": "/s", "op": "gt", "value": 1})));
    // contains: strings case-insensitively, arrays by element (objects as subsets).
    assert!(passes(&v, json!({"path": "/s", "op": "contains", "value": "WORLD"})));
    assert!(!passes(&v, json!({"path": "/s", "op": "contains", "value": "moon"})));
    assert!(passes(&v, json!({"path": "/a", "op": "contains", "value": 2})));
    assert!(passes(&v, json!({"path": "/layers", "op": "contains", "value": {"name": "title"}})));
    assert!(!passes(&v, json!({"path": "/layers", "op": "contains", "value": {"name": "Logo", "kind": "text"}})));
    // matches: a case-insensitive glob.
    assert!(passes(&v, json!({"path": "/s", "op": "matches", "value": "hello*"})));
    assert!(passes(&v, json!({"path": "/s", "op": "matches", "value": "h?llo w*d"})));
    assert!(!passes(&v, json!({"path": "/s", "op": "matches", "value": "*moon*"})));
    // exists / absent (null counts as absent).
    assert!(passes(&v, json!({"path": "/o/k", "op": "exists"})));
    assert!(!passes(&v, json!({"path": "/nul", "op": "exists"})));
    assert!(passes(&v, json!({"path": "/nul", "op": "absent"})));
    assert!(passes(&v, json!({"path": "/missing", "op": "absent"})));
    assert!(!passes(&v, json!({"path": "/n", "op": "absent"})));
    // len, lenGte, lenLte and `length` with any compare.
    assert!(passes(&v, json!({"path": "/a", "op": "len", "value": 3})));
    assert!(passes(&v, json!({"path": "/s", "op": "lenGte", "value": 11})));
    assert!(!passes(&v, json!({"path": "/o", "op": "lenLte", "value": 0})));
    assert!(passes(&v, json!({"path": "/layers", "length": true, "op": "gt", "value": 1})));
    assert!(!passes(&v, json!({"path": "/n", "op": "len", "value": 1})));
    // in / notIn.
    assert!(passes(&v, json!({"path": "/n", "op": "in", "value": [1, 3]})));
    assert!(passes(&v, json!({"path": "/s", "op": "notIn", "value": ["x"]})));
    // any / all / none, with paths relative to the element.
    assert!(passes(
        &v,
        json!({"path": "/layers", "op": "any", "value": [{"path": "/name", "op": "eq", "value": "Title"}, {"path": "/kind", "op": "eq", "value": "text"}]})
    ));
    assert!(!passes(
        &v,
        json!({"path": "/layers", "op": "any", "value": [{"path": "/name", "op": "eq", "value": "Title"}, {"path": "/kind", "op": "eq", "value": "solid"}]})
    ));
    assert!(passes(&v, json!({"path": "/layers", "op": "all", "value": {"path": "/name", "op": "exists"}})));
    assert!(!passes(&v, json!({"path": "/layers", "op": "all", "value": {"path": "/kind", "op": "eq", "value": "text"}})));
    assert!(passes(&v, json!({"path": "/layers", "op": "none", "value": {"path": "/kind", "op": "eq", "value": "shape"}})));
    assert!(!passes(&v, json!({"path": "/a", "op": "none", "value": {"op": "gt", "value": 2}})));
    assert!(passes(&v, json!({"path": "/a", "op": "all", "value": {"op": "lte", "value": 3}})));
    // Path alternatives: the first present one.
    assert!(passes(&v, json!({"path": ["/evaluated", "/n"], "op": "eq", "value": 3})));
}

#[test]
fn failures_explain_expected_and_actual() {
    let v = json!({"layers": [{"name": "Logo", "x": 10}]});
    let o = exp(&v, json!({"path": "/layers/0/x", "op": "gte", "value": 100}));
    assert!(!o.pass && !o.malformed);
    assert_eq!(o.reason, "expected /layers/0/x ≥ 100, got 10");
    assert_eq!(o.actual, json!(10));
    let o = exp(&v, json!({"path": "/layers/0/y", "op": "eq", "value": 1}));
    assert!(o.reason.contains("/layers/0/y is not in the result"), "{}", o.reason);
    let o = exp(&v, json!({"path": "/layers", "op": "any", "value": {"path": "/name", "op": "eq", "value": "Title"}}));
    assert!(o.reason.contains("found none among 1 element(s)"), "{}", o.reason);
    let o = exp(&v, json!({"path": "/layers", "op": "none", "value": {"path": "/name", "op": "eq", "value": "Logo"}}));
    assert!(o.reason.contains("found 1"), "{}", o.reason);
    assert_eq!(o.actual, json!([{"name": "Logo", "x": 10}]));
}

#[test]
fn json_pointer_and_select() {
    let v = json!({"layers": [{"name": "A", "id": 1, "box": {"w": 5}}, {"name": "B", "id": 2, "box": {"w": 7}}, {"name": "B", "id": 3, "box": {"w": 9}}], "a/b": {"c~d": 4}});
    // RFC 6901 escapes.
    assert!(passes(&v, json!({"path": "/a~1b/c~0d", "op": "eq", "value": 4})));
    assert!(passes(&v, json!({"path": "", "op": "exists"})));
    let sel = |steps: Value| exam::select_chain(&v, &serde_json::from_value::<Vec<Value>>(steps).unwrap()).map(|x| x["id"].clone());
    assert_eq!(sel(json!([{"array": "/layers", "where": {"name": "B"}}])), Ok(json!(2)));
    assert_eq!(sel(json!([{"array": "/layers", "where": {"name": "B"}, "index": 1}])), Ok(json!(3)));
    assert_eq!(sel(json!([{"array": "/layers", "where": {"/box/w": 9}}])), Ok(json!(3)));
    assert_eq!(sel(json!([{"array": "/layers", "index": 0}])), Ok(json!(1)));
    let e = sel(json!([{"array": "/layers", "where": {"name": "C"}}])).unwrap_err();
    assert!(e.contains("no element of /layers with name = \"C\"") && e.contains("\"A\", \"B\""), "{e}");
    assert!(sel(json!([{"array": "/nope"}])).unwrap_err().contains("not found"));
    assert!(sel(json!([{"array": "/layers", "bogus": 1}])).unwrap_err().contains("unknown `select` field"));
    // A check-level select makes the expectation paths relative; an expectation may select too.
    let c = Check {
        id: "c".into(),
        desc: String::new(),
        query: "comp.info".into(),
        params: json!({}),
        select: vec![json!({"array": "/layers", "where": {"name": "B"}})],
        expect: vec![json!({"path": "/box/w", "op": "eq", "value": 7})],
        show: None,
    };
    assert!(exam::evaluate(&c, &v).pass);
    let c2 = Check { select: vec![], expect: vec![json!({"select": {"array": "/layers", "index": 2}, "path": "/id", "op": "eq", "value": 3})], ..c.clone() };
    assert!(exam::evaluate(&c2, &v).pass);
    let c3 = Check { select: vec![json!({"array": "/layers", "where": {"name": "Z"}})], ..c };
    let o = exam::evaluate(&c3, &v);
    assert!(!o.pass && o.reason.contains("no element"), "{o:?}");
}

#[test]
fn glob_and_score() {
    assert!(exam::glob("*", ""));
    assert!(exam::glob("a*b*c", "aXXbYYc"));
    assert!(!exam::glob("a*b*c", "aXXbYY"));
    assert!(exam::glob("**x", "x"));
    assert!(exam::glob("WIGGLE(*)", "wiggle(2, 30)"));
    assert_eq!(exam::score(0, 0), 0.0);
    assert_eq!(exam::score(2, 3), 0.6667);
    assert_eq!(exam::score(3, 3), 1.0);
}

fn expand(c: Value) -> Check {
    exam::expand(&c, 0, Some("/evals")).unwrap_or_else(|e| panic!("{c}: {e}"))
}

#[test]
fn lint_shorthand_expands_to_render_lint() {
    let c = expand(json!({"id": "clean", "desc": "no problems", "lint": {"comp": "Main", "rules": ["cut-off"]}}));
    assert_eq!(c.id, "clean");
    assert_eq!(c.desc, "no problems");
    assert_eq!(c.query, "render.lint");
    assert_eq!(c.params, json!({"comp": "Main", "rules": ["cut-off"]}));
    assert_eq!(c.expect, vec![json!({"path": "/problems", "op": "none", "value": [{"path": "/severity", "op": "in", "value": ["error", "warning"]}]})]);
    let c = expand(json!({"lint": {}, "maxSeverity": "warning", "allow": ["off-beat"]}));
    assert_eq!(
        c.expect,
        vec![
            json!({"path": "/problems", "op": "none", "value": [{"path": "/severity", "op": "in", "value": ["error"]}, {"path": "/rule", "op": "notIn", "value": ["off-beat"]}]})
        ]
    );
    assert_eq!(c.id, "check1");
    // The expansion evaluated on a render.lint-shaped result.
    let res = json!({"clean": false, "counts": {"error": 0, "warning": 1, "info": 1}, "problems": [
        {"rule": "off-beat", "severity": "warning", "layer": 2, "layerName": "Cut", "time": 1.0, "message": "…"},
        {"rule": "text-too-small", "severity": "info", "layer": 3, "layerName": "Fine print", "time": 0.0, "message": "…"}]});
    assert!(exam::evaluate(&c, &res).pass);
    let strict = expand(json!({"lint": {}}));
    let o = exam::evaluate(&strict, &res);
    assert!(!o.pass && o.reason.contains("found 1"), "{o:?}");
    assert!(!exam::evaluate(&expand(json!({"lint": {}, "maxSeverity": "none"})), &res).pass);
}

#[test]
fn motion_shorthand_expands_to_comp_motion() {
    let c = expand(
        json!({"id": "fly", "motion": {"comp": "Main"}, "layer": "Logo", "segment": {"kind": "enter", "edge": "left", "startApprox": 0, "endApprox": 1, "tol": 0.2}}),
    );
    assert_eq!(c.query, "comp.motion");
    assert_eq!(c.params, json!({"comp": "Main", "layers": ["Logo"]}));
    assert_eq!(c.select, vec![json!({"array": "/layers", "where": {"name": "Logo"}})]);
    assert_eq!(
        c.expect,
        vec![json!({"path": "/segments", "op": "any", "value": [
            {"path": "/kind", "op": "eq", "value": "enter"},
            {"path": "/edge", "op": "eq", "value": "left"},
            {"path": "/start", "op": "approx", "value": 0, "tol": 0.2},
            {"path": "/end", "op": "approx", "value": 1, "tol": 0.2}]})]
    );
    // An id selects by `layer`, "#n" takes the one layer asked for; a segment list wants each.
    let c = expand(json!({"motion": {}, "layer": 7, "segment": [{"kind": "enter"}, {"kind": "hold"}]}));
    assert_eq!(c.select, vec![json!({"array": "/layers", "where": {"layer": 7}})]);
    assert_eq!(c.expect.len(), 2);
    assert_eq!(expand(json!({"motion": {}, "layer": "#2", "segment": {"kind": "exit"}})).select, vec![json!({"array": "/layers", "index": 0})]);
    // Evaluated on a comp.motion-shaped result.
    let fly = expand(json!({"motion": {}, "layer": "Logo", "segment": {"kind": "enter", "edge": "left", "startApprox": 0, "endApprox": 1, "tol": 0.2}}));
    let res = json!({"comp": 1, "layers": [{"layer": 2, "name": "Logo", "segments": [
        {"kind": "enter", "start": 0.0, "end": 1.04, "from": [-200, 540], "to": [960, 540], "edge": "left"},
        {"kind": "hold", "start": 1.04, "end": 5.0, "from": [960, 540], "to": [960, 540]}]}], "overlaps": []});
    assert!(exam::evaluate(&fly, &res).pass);
    let right = expand(json!({"motion": {}, "layer": "Logo", "segment": {"kind": "enter", "edge": "right"}}));
    let o = exam::evaluate(&right, &res);
    assert!(!o.pass && o.reason.contains("found none"), "{o:?}");
    assert_eq!(o.actual, res["layers"], "`show` reports the layers");
    for bad in [
        json!({"motion": {}, "segment": {"kind": "enter"}}),
        json!({"motion": {}, "layer": "L"}),
        json!({"motion": {}, "layer": "L", "segment": {"edge": "left"}}),
        json!({"motion": {}, "layer": "L", "segment": {"kind": "enter", "speed": 1}}),
    ] {
        assert!(exam::expand(&bad, 0, None).is_err(), "{bad}");
    }
}

#[test]
fn compare_prop_layer_count_and_comp_shorthands_expand() {
    let c = expand(json!({"compare": {"reference": "ref/title.png", "comp": "Main", "time": 1.5, "threshold": 4}}));
    assert_eq!(c.query, "render.compare");
    assert_eq!(c.params, json!({"a": {"comp": "Main", "time": 1.5}, "b": {"path": "/evals/ref/title.png"}, "threshold": 4}));
    assert_eq!(c.expect, vec![json!({"path": "/pass", "op": "eq", "value": true})]);
    let c = expand(json!({"compare": {"reference": "/abs/ref.png", "minPsnr": 30, "maxSide": 512, "maxChangedPct": 1.5}}));
    assert_eq!(c.params, json!({"a": {}, "b": {"path": "/abs/ref.png"}, "min_psnr": 30, "max_side": 512}));
    assert_eq!(c.expect, vec![json!({"path": "/changed_pct", "op": "lte", "value": 1.5})]);
    assert!(exam::expand(&json!({"compare": {"reference": "r.png", "diff_path": "x"}}), 0, None).is_err());

    let c = expand(json!({"prop": {"comp": "Main", "layer": "Logo", "path": "transform/opacity", "time": 2}, "op": "approx", "value": 50, "tol": 1}));
    assert_eq!(c.query, "prop.get");
    assert_eq!(c.params, json!({"comp": "Main", "layer": "Logo", "path": "transform/opacity", "time": 2}));
    assert_eq!(c.expect, vec![json!({"path": ["/evaluated", "/value"], "op": "approx", "value": 50, "tol": 1})]);
    let c = expand(json!({"prop": {"layer": "Logo", "path": "transform/position", "field": "keys"}, "op": "lenGte", "value": 2}));
    assert_eq!(c.expect, vec![json!({"path": "/keys", "op": "lenGte", "value": 2})]);

    let c = expand(json!({"layerCount": {"comp": "Main"}, "op": "eq", "value": 2}));
    assert_eq!((c.query.as_str(), &c.params), ("comp.info", &json!({"comp": "Main"})));
    assert_eq!(c.expect, vec![json!({"path": "/layers", "op": "eq", "value": 2, "length": true})]);

    let c = expand(json!({"comp": {"name": "Main"}, "width": 1920, "fps": 25}));
    assert_eq!(c.params, json!({"comp": "Main"}));
    assert_eq!(c.expect, vec![json!({"path": "/width", "op": "eq", "value": 1920}), json!({"path": "/frameRate", "op": "approx", "value": 25, "tol": 0.01})]);
}

#[test]
fn prop_layer_count_and_comp_checks_end_to_end() {
    let mut s = project();
    s.execute("prop.addKey", json!({"layer": "Logo", "path": "transform/opacity", "time": 0, "value": 0})).unwrap();
    s.execute("prop.addKey", json!({"layer": "Logo", "path": "transform/opacity", "time": 2, "value": 100})).unwrap();
    s.execute("layer.setParent", json!({"layers": ["Logo"], "parent": "Ctrl"})).unwrap();
    let r = run(
        &mut s,
        json!([
            {"id": "mid", "prop": {"comp": "Main", "layer": "Logo", "path": "transform/opacity", "time": 1}, "op": "approx", "value": 50, "tol": 1},
            {"id": "keys", "prop": {"layer": "Logo", "path": "transform/opacity", "field": "keys"}, "op": "lenGte", "value": 2},
            {"id": "anchor", "prop": {"layer": "Logo", "path": "transform/anchor"}, "op": "approx", "value": [100, 50]},
            {"id": "count", "layerCount": {"comp": "Main"}, "op": "eq", "value": 2},
            {"id": "comp", "comp": {"name": "Main"}, "width": 1920, "height": 1080, "fps": 25, "duration": 5},
            {"id": "parent", "query": "comp.info", "params": {"comp": "Main"}, "select": {"array": "/layers", "where": {"name": "Logo"}}, "expect": [{"path": "/parent", "op": "exists"}]},
            {"id": "bounds", "query": "layer.bounds", "params": {"layers": ["Logo"]}, "expect": [{"path": "/layers/0/bounds", "op": "approx", "value": [860, 490, 200, 100], "tol": 0.5}]}
        ]),
    );
    assert_eq!(r["passed"], true, "{r:#}");
    assert_eq!((r["total"].clone(), r["passedCount"].clone(), r["score"].clone()), (json!(7), json!(7), json!(1.0)));

    // Failing versions say why.
    let bad = one(&mut s, json!({"id": "count", "desc": "three layers", "layerCount": {"comp": "Main"}, "op": "eq", "value": 3}));
    assert_eq!(bad["pass"], false);
    assert_eq!(bad["desc"], "three layers");
    assert_eq!(bad["reason"], "expected length of /layers = 3, got 2");
    assert_eq!(bad["actual"].as_array().map(Vec::len), Some(2), "`show` reports the layers: {bad}");
    let bad = one(&mut s, json!({"comp": {"name": "Main"}, "width": 1080}));
    assert_eq!(bad["reason"], "expected /width = 1080, got 1920");
    let bad = one(&mut s, json!({"comp": {"name": "Vertical"}}));
    assert!(bad["reason"].as_str().unwrap().starts_with("comp.info failed:"), "{bad}");
    let bad = one(&mut s, json!({"prop": {"layer": "Nope", "path": "transform/opacity"}, "op": "eq", "value": 1}));
    assert!(bad["reason"].as_str().unwrap().contains("prop.get failed"), "{bad}");
}

#[test]
fn compare_check_end_to_end() {
    let mut s = project();
    let dir = std::env::temp_dir().join(format!("ec-exam-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h, rgba) = s.render_rgba8(s.active_comp_id().unwrap(), effectcraft_time::Tick::ZERO, 0).unwrap();
    let png = crate::commands::comp_more::encode_png(&rgba, w, h).unwrap();
    std::fs::write(dir.join("ref.png"), png).unwrap();
    let base = dir.to_string_lossy().into_owned();
    let r = s.execute("exam.check", json!({"checks": [{"id": "same", "compare": {"reference": "ref.png", "time": 0}}], "base": base})).unwrap();
    assert_eq!(r["passed"], true, "{r:#}");
    s.execute("prop.set", json!({"layer": "Logo", "path": "transform/position", "value": [100, 100, 0]})).unwrap();
    let r = s.execute("exam.check", json!({"checks": [{"id": "same", "compare": {"reference": "ref.png", "time": 0}}], "base": base})).unwrap();
    assert_eq!(r["passed"], false);
    assert!(r["results"][0]["reason"].as_str().unwrap().contains("/pass = true, got false"), "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_read_only_queries_run() {
    let mut s = project();
    let before = s.revision;
    let r = one(&mut s, json!({"id": "edit", "query": "layer.newSolid", "params": {"name": "X"}, "expect": [{"path": "/layer", "op": "exists"}]}));
    assert_eq!(r["pass"], false);
    assert!(r["reason"].as_str().unwrap().contains("changes the project"), "{r}");
    for (q, params, why) in [
        ("engine.batch", json!({"steps": []}), "not a read-only query"),
        ("journal.export", json!({}), "not a read-only query"),
        ("render.frame", json!({}), "not a read-only query"),
        ("exam.check", json!({"checks": []}), "not a read-only query"),
        ("render.compare", json!({"a": {}, "b": {}, "diff_path": "/tmp/x.png"}), "writes a file"),
        ("no.such.command", json!({}), "unknown command"),
    ] {
        let r = one(&mut s, json!({"query": q, "params": params, "expect": [{"op": "exists"}]}));
        assert_eq!(r["pass"], false, "{q}");
        assert!(r["reason"].as_str().unwrap().contains(why), "{q}: {r}");
    }
    assert_eq!(s.revision, before, "nothing changed the project");
    assert_eq!(s.project.comp(s.active_comp_id().unwrap()).unwrap().layers.len(), 2);
    // Every journaled command is refused; the deny list names only real commands.
    for id in ["edit.undo", "prop.set", "file.save"] {
        assert!(crate::commands::exam_cmds::refused(id, &json!({})).is_some(), "{id}");
    }
    assert!(crate::commands::exam_cmds::refused("comp.info", &json!({})).is_none());
}

#[test]
fn malformed_checks_fail_alone() {
    let mut s = project();
    let r = run(
        &mut s,
        json!([
            "not an object",
            {"id": "nokind"},
            {"id": "two", "prop": {"layer": "Logo", "path": "transform/opacity"}, "layerCount": {}, "op": "eq", "value": 1},
            {"id": "noexpect", "query": "comp.info"},
            {"id": "badop", "query": "comp.info", "expect": [{"path": "/width", "op": "bigger", "value": 1}]},
            {"id": "novalue", "query": "comp.info", "expect": [{"path": "/width", "op": "eq"}]},
            {"id": "badfield", "query": "comp.info", "expect": [{"path": "/width", "op": "eq", "value": 1, "colour": 1}]},
            {"id": "badpath", "query": "comp.info", "expect": [{"path": 5, "op": "exists"}]},
            {"id": "badparam", "query": "comp.info", "params": {"bogus": 1}, "expect": [{"op": "exists"}]},
            {"id": "badselect", "query": "comp.info", "select": 3, "expect": [{"op": "exists"}]},
            {"id": "badany", "query": "comp.info", "expect": [{"path": "/layers", "op": "any", "value": {"path": "/name", "op": "wat"}}]},
            {"id": "extra", "layerCount": {}, "op": "eq", "value": 2, "field": "x"},
            {"id": "ok", "query": "comp.info", "expect": [{"path": "/width", "op": "eq", "value": 1920}]}
        ]),
    );
    let results = r["results"].as_array().unwrap();
    assert_eq!(results.len(), 13);
    let reason = |i: usize| results[i]["reason"].as_str().unwrap().to_string();
    assert_eq!(results[0]["id"], "check1");
    for (i, want) in [
        (0, "must be an object"),
        (1, "needs one of"),
        (2, "takes one kind"),
        (3, "missing `expect`"),
        (4, "unknown op `bigger`"),
        (5, "needs a `value`"),
        (6, "unknown expectation field `colour`"),
        (7, "`path` must be a JSON Pointer"),
        (8, "unknown parameter"),
        (9, "`select` must be"),
        (10, "unknown op `wat`"),
        (11, "unknown field `field` for a `layerCount` check"),
    ] {
        assert_eq!(results[i]["pass"], false, "{i}");
        assert!(reason(i).contains(want), "{i}: {}", reason(i));
    }
    assert_eq!(results[12]["pass"], true);
    assert_eq!(r["passed"], false);
    assert_eq!(r["passedCount"], 1);
    assert_eq!(r["score"], exam::score(1, 13));

    // `any` / `all` / `none` nest at most six deep.
    let deep = json!([[[[[[[[1]]]]]]]]);
    let mut e = json!({"op": "exists"});
    for _ in 0..7 {
        e = json!({"op": "any", "value": e});
    }
    let o = exp(&deep, e);
    assert!(o.malformed && o.reason.contains("nest at most"), "{o:?}");
    // An unknown `prop` field.
    assert!(
        exam::expand(&json!({"prop": {"layer": "L", "path": "p", "frame": 1}, "op": "eq", "value": 1}), 0, None)
            .unwrap_err()
            .contains("unknown `prop` field `frame`")
    );

    // Whole-command errors are only for the wrong top-level shape.
    assert!(s.execute("exam.check", json!({})).is_err());
    assert!(s.execute("exam.check", json!({"checks": {}})).is_err());
    assert!(s.execute("exam.check", json!({"task": {"id": "t"}})).is_err());
    assert!(s.execute("exam.check", json!({"task": {"checks": []}, "checks": []})).is_err());
    let many: Vec<Value> = (0..=exam::MAX_CHECKS).map(|_| json!({"layerCount": {}, "op": "gte", "value": 0})).collect();
    assert!(s.execute("exam.check", json!({"checks": many})).unwrap_err().to_string().contains("at most"));
    // No checks: not passed, score 0.
    let r = run(&mut s, json!([]));
    assert_eq!((r["passed"].clone(), r["score"].clone()), (json!(false), json!(0.0)));
}

#[test]
fn task_objects_are_scored() {
    let mut s = project();
    let task = json!({"id": "demo", "title": "Demo", "prompt": "…", "limits": {"minutes": 1, "toolCalls": 5}, "checks": [
        {"id": "a", "layerCount": {}, "op": "eq", "value": 2},
        {"id": "b", "layerCount": {}, "op": "eq", "value": 5},
        {"id": "c", "comp": {"name": "Main"}, "height": 1080},
        {"id": "d", "comp": {"name": "Main"}, "height": 1920}
    ]});
    let r = s.execute_checked("exam.check", json!({"task": task})).unwrap();
    assert_eq!(r["task"], "demo");
    assert_eq!(r["passed"], false);
    assert_eq!((r["total"].clone(), r["passedCount"].clone(), r["score"].clone()), (json!(4), json!(2), json!(0.5)));
    let ids: Vec<&str> = r["results"].as_array().unwrap().iter().filter(|x| x["pass"] == false).map(|x| x["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["b", "d"]);
}

/// The query ids exam checks may need but this build doesn't have yet (other branches add them).
fn missing_query(c: &Value) -> Option<String> {
    let chk = exam::expand(c, 0, None).ok()?;
    crate::commands::find(&chk.query).is_none().then_some(chk.query)
}

/// Every task in `evals/agent/tasks`: `setup` builds a start that does not pass, and `solution`
/// makes every check pass. Every check runs: a check whose query this build lacks fails the test.
#[test]
fn every_task_is_solvable() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/agent/tasks");
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    assert!(files.len() >= 15, "{} tasks", files.len());
    let mut skipped = std::collections::BTreeSet::new();
    for f in files {
        let task: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        let id = task["id"].as_str().unwrap().to_string();
        let checks: Vec<Value> = task["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| match missing_query(c) {
                Some(q) => {
                    skipped.insert(q);
                    false
                }
                None => true,
            })
            .cloned()
            .collect();
        let mut s = Session::default();
        for st in task["setup"].as_array().into_iter().flatten() {
            s.execute_checked(st["command"].as_str().unwrap(), st.get("params").cloned().unwrap_or(json!({})))
                .unwrap_or_else(|e| panic!("{id} setup {st}: {e}"));
        }
        let base = f.parent().unwrap().to_string_lossy().into_owned();
        let start = s.execute("exam.check", json!({"checks": checks, "base": base})).unwrap();
        assert_eq!(start["passed"], false, "{id}: the starting project already passes");
        for st in task["solution"].as_array().unwrap_or_else(|| panic!("{id}: no solution")) {
            s.execute_checked(st["command"].as_str().unwrap(), st.get("params").cloned().unwrap_or(json!({})))
                .unwrap_or_else(|e| panic!("{id} solution {st}: {e}"));
        }
        let r = s.execute("exam.check", json!({"checks": checks, "base": base})).unwrap();
        let failed: Vec<String> = r["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["pass"] != true)
            .map(|x| format!("{}: {} (actual {})", x["id"], x["reason"], x["actual"]))
            .collect();
        assert!(failed.is_empty(), "{id}: the solution fails\n  {}", failed.join("\n  "));
    }
    assert!(skipped.is_empty(), "checks whose query is not in this build: {skipped:?}");
}

#[test]
fn lint_check_end_to_end() {
    let mut s = project();
    let t = s.execute("layer.newText", json!({"name": "Fine print", "text": "Hello", "size": 8, "fill": "#ffffff", "position": [960, 540]})).unwrap()["layer"]
        .clone();
    let check = json!({"id": "legible", "lint": {"comp": "Main", "time": 0, "rules": ["text-too-small"]}});
    let r = one(&mut s, check.clone());
    assert_eq!(r["pass"], false, "{r}");
    s.execute("layer.setText", json!({"layer": t, "size": 72})).unwrap();
    let r = one(&mut s, check);
    assert_eq!(r["pass"], true, "{r}");
}

#[test]
fn motion_check_end_to_end() {
    let mut s = project();
    let check = json!({"id": "fly", "motion": {"comp": "Main"}, "layer": "Logo",
        "segment": {"kind": "enter", "edge": "left", "startApprox": 0, "endApprox": 1, "tol": 0.2}});
    assert_eq!(one(&mut s, check.clone())["pass"], false);
    for (time, x) in [(0.0, -200.0), (1.0, 960.0)] {
        s.execute("prop.addKey", json!({"layer": "Logo", "path": "transform/position", "time": time, "value": [x, 540.0]})).unwrap();
    }
    let r = one(&mut s, check);
    assert_eq!(r["pass"], true, "{r}");
}
