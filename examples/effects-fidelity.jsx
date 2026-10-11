// Effect fidelity scenes (gaps.md G1): one original test scene per case, built with the After
// Effects scripting object model so the same file runs in After Effects (through AEsync) and in
// EffectCraft (`effectcraft-cli script`). Each case renders one frame to a PNG; the driver,
// examples/check-ae-effects.mjs, compares the two apps' frames and writes the scores.
//
// Clean room: the scene is generated (Gradient Ramp and Checkerboard on solids, no footage, no
// text, so fonts can't differ); no Adobe assets. The driver prepends
//   var EC_FIDELITY = {out: "/abs/folder", only: ["name", ...] | null, keep: false};
// Cases name effects by match name and parameters by their English display name.

var CASES = [
  { name: "gaussian-blur-10", effect: "ADBE Gaussian Blur 2", params: { "Blurriness": 10 } },
  { name: "gaussian-blur-40-repeat", effect: "ADBE Gaussian Blur 2", params: { "Blurriness": 40, "Repeat Edge Pixels": 1 } },
  { name: "fast-box-blur-8", effect: "ADBE Box Blur2", params: { "Blur Radius": 8 } },
  { name: "directional-blur", effect: "ADBE Motion Blur", params: { "Direction": 45, "Blur Length": 20 } },
  { name: "radial-blur-spin", effect: "ADBE Radial Blur", params: { "Amount": 20 } },
  { name: "glow-default", effect: "ADBE Glo2", params: {} },
  { name: "glow-strong", effect: "ADBE Glo2", params: { "Glow Threshold": 40, "Glow Radius": 40, "Glow Intensity": 2 } },
  { name: "brightness-contrast", effect: "ADBE Brightness & Contrast 2", params: { "Brightness": 30, "Contrast": 25 } },
  { name: "hue-saturation", effect: "ADBE HUE SATURATION", params: { "Master Hue": 60, "Master Saturation": 30 } },
  { name: "exposure-plus-1", effect: "ADBE Exposure2", params: { "Exposure": 1 } },
  { name: "tint-half", effect: "ADBE Tint", params: { "Amount to Tint": 50 } },
  { name: "invert", effect: "ADBE Invert", params: {} },
  { name: "posterize-4", effect: "ADBE Posterize", params: { "Level": 4 } },
  { name: "fill-red", effect: "ADBE Fill", params: { "Color": [1, 0, 0, 1], "Opacity": 0.5 } },
  { name: "mosaic-16x9", effect: "ADBE Mosaic", params: { "Horizontal Blocks": 16, "Vertical Blocks": 9 } },
  { name: "find-edges", effect: "ADBE Find Edges", params: {} },
  { name: "twirl-90", effect: "ADBE Twirl", params: { "Angle": 90 } },
  { name: "bulge-default", effect: "ADBE Bulge", params: {} },
  { name: "wave-warp-default", effect: "ADBE Wave Warp", params: {} },
  { name: "turbulent-displace-default", effect: "ADBE Turbulent Displace", params: {} },
  { name: "fractal-noise-default", effect: "ADBE Fractal Noise", params: {} },
  { name: "linear-wipe-40", effect: "ADBE Linear Wipe", params: { "Transition Completion": 40 } },
  { name: "drop-shadow-on-layer", effect: "ADBE Drop Shadow", params: { "Distance": 10, "Softness": 8 }, target: "checker" }
];

var W = 320, H = 180;

function setParams(fx, params) {
  for (var k in params) {
    if (!params.hasOwnProperty(k)) continue;
    var p = fx.property(k);
    if (!p) throw new Error("no parameter \"" + k + "\" on " + fx.matchName);
    p.setValue(params[k]);
  }
}

function buildCase(c) {
  var comp = app.project.items.addComp("ECFID " + c.name, W, H, 1, 1, 30);
  var bg = comp.layers.addSolid([0, 0, 0], "Ramp", W, H, 1);
  var ramp = bg.property("ADBE Effect Parade").addProperty("ADBE Ramp");
  setParams(ramp, { "Start of Ramp": [0, 0], "Start Color": [0.1, 0.2, 0.8, 1], "End of Ramp": [W, H], "End Color": [1, 0.8, 0.1, 1] });
  var ch = comp.layers.addSolid([1, 1, 1], "Checker", 160, 90, 1);
  var chk = ch.property("ADBE Effect Parade").addProperty("ADBE Checkerboard");
  setParams(chk, { "Anchor": [0, 0], "Width": 16, "Color": [0.9, 0.1, 0.2, 1] });
  ch.property("ADBE Transform Group").property("ADBE Rotate Z").setValue(15);
  var target;
  if (c.target === "checker") {
    target = ch;
  } else {
    target = comp.layers.addSolid([1, 1, 1], "Adjust", W, H, 1);
    target.adjustmentLayer = true;
  }
  var fx = target.property("ADBE Effect Parade").addProperty(c.effect);
  setParams(fx, c.params);
  return comp;
}

function waitFor(file) {
  for (var i = 0; i < 200 && !file.exists; i++) {
    if (typeof $ !== "undefined" && $.sleep) $.sleep(50);
  }
  return file.exists;
}

function runCases(cfg) {
  var out = [], made = [];
  var folder = new Folder(cfg.out);
  if (!folder.exists) folder.create();
  for (var i = 0; i < CASES.length; i++) {
    var c = CASES[i];
    if (cfg.only && cfg.only.length) {
      var want = false;
      for (var j = 0; j < cfg.only.length; j++) if (cfg.only[j] === c.name) want = true;
      if (!want) continue;
    }
    var rec = { name: c.name, effect: c.effect, params: c.params, target: c.target || "adjustment" };
    try {
      var comp = buildCase(c);
      made.push(comp);
      var f = new File(cfg.out + "/" + c.name + ".png");
      if (f.exists) f.remove();
      comp.saveFrameToPng(0.5, f);
      rec.file = f.fsName;
      rec.ok = waitFor(f);
      if (!rec.ok) rec.error = "the frame was not written";
    } catch (e) {
      rec.ok = false;
      rec.error = String(e);
    }
    out.push(rec);
  }
  if (!cfg.keep) {
    for (var k = made.length - 1; k >= 0; k--) {
      try {
        var cm = made[k], solids = [];
        for (var n = 1; n <= cm.numLayers; n++) solids.push(cm.layer(n).source);
        cm.remove();
        for (var m = 0; m < solids.length; m++) if (solids[m]) solids[m].remove();
      } catch (e2) {}
    }
  }
  return { app: app.version, cases: out };
}

var __result = runCases(typeof EC_FIDELITY !== "undefined" ? EC_FIDELITY : { out: (typeof Folder.temp !== "undefined" ? Folder.temp.fsName : "/tmp") + "/ec-fidelity", only: null, keep: false });
// AEsync returns the last expression; EffectCraft's CLI prints it.
JSON.stringify(__result);
