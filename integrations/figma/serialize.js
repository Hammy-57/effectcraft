// EffectCraft Figma serializer: Figma nodes → the node JSON EffectCraft's `file.importFigma`
// reads (the shape of Figma's REST API `nodes` reply, plus plug-in-only fields such as
// `absoluteTransform`, `vectorPaths` and `pointCount`; default values left out). One copy runs in
// two places:
//
// - the EffectCraft Transfer plug-in (plugin/code.js embeds this file; build-plugin.sh), and
// - the Figma MCP's `use_figma` tool (agents, no token, replies up to 20 KB): send this file's
//   text followed by
//     return await effectcraftSerialize({ ids: ['12:345'], fileKey: 'FILEKEY', images: 'none', budget: 18000 });
//   then, while any reply lists ids in `pending`, the same with { subtree: [those ids], images:
//   'none', budget: 18000 }.
//   Save every reply as a file and import them together: file.importFigma {payloadPaths: [...]}.
//
// effectcraftSerialize(opts) → { source, fileKey, nodes | fragments, images, rasterize, pending,
//                                missing, stats }
//   opts.ids        node ids ('12:345' or '12-345'); default: the current selection
//   opts.subtree    node id(s): reply with { fragments: { id: node, … } } (parts of a split
//                   payload; with a budget, the ids that don't fit wait in `pending`)
//   opts.fileKey    recorded as the payload's `fileKey` (EffectCraft names the asset folder after it)
//   opts.images     what to embed for nodes that come in as images (base64, from exportAsync):
//                   'both' (default: PDF, sharp at any scale, plus a 2x PNG fallback), 'pdf',
//                   'png', or 'none' (list them in `rasterize` only; agents fetch them by URL)
//   opts.imagesOnly ids: reply with only { images } for these nodes (use with images: 'pdf' …)
//   opts.budget     max reply size in characters (default unlimited): subtrees that don't fit are
//                   replaced by stubs { "$ref": id } and listed in `pending`; a single node that
//                   can't fit comes in as an image (reason 'too large to transfer')
//   opts.scale      PNG scale (default 2)
//   opts.onProgress function (text) for progress messages (the plug-in's panel)
// `rasterize`: every node that comes in as an image, {id, name, reason}. `missing`: ids not found.
// Written against Figma's public Plugin API documentation.

var EC_NATIVE_TYPES = {
  FRAME: 1, GROUP: 1, COMPONENT: 1, COMPONENT_SET: 1, INSTANCE: 1, SECTION: 1, BOOLEAN_OPERATION: 1,
  RECTANGLE: 1, ELLIPSE: 1, LINE: 1, VECTOR: 1, STAR: 1, POLYGON: 1, TEXT: 1, SLICE: 1
};
var EC_IMAGE_PAINTS = { IMAGE: 1, VIDEO: 1, EMOJI: 1, PATTERN: 1 };
var EC_MAX_NODES = 20000;

function ecMixed(v) { return v === figma.mixed || typeof v === 'symbol'; }

// Numbers rounded to 1e-4: smaller payloads, no visible change.
function ecRound(x) { return typeof x === 'number' && isFinite(x) ? Math.round(x * 10000) / 10000 : x; }

function ecRgba(c, a) {
  return { r: ecRound(c.r), g: ecRound(c.g), b: ecRound(c.b), a: ecRound(c.a !== undefined ? c.a : (a !== undefined ? a : 1)) };
}

// Plug-in gradients carry `gradientTransform` (node's normalised space → gradient space);
// the REST format carries handle positions in the node's normalised space.
function ecInvert(m) {
  var a = m[0][0], c = m[0][1], e = m[0][2], b = m[1][0], d = m[1][1], f = m[1][2];
  var det = a * d - b * c;
  if (Math.abs(det) < 1e-12) return [[1, 0, 0], [0, 1, 0]];
  return [[d / det, -c / det, (c * f - d * e) / det], [-b / det, a / det, (b * e - a * f) / det]];
}
function ecApply(m, x, y) { return { x: ecRound(m[0][0] * x + m[0][1] * y + m[0][2]), y: ecRound(m[1][0] * x + m[1][1] * y + m[1][2]) }; }

function ecPaint(p) {
  var out = { type: p.type };
  if (p.visible === false) out.visible = false;
  if (p.opacity !== undefined && p.opacity !== 1) out.opacity = ecRound(p.opacity);
  if (p.blendMode && p.blendMode !== 'NORMAL') out.blendMode = p.blendMode;
  if (p.type === 'SOLID') {
    out.color = ecRgba(p.color, 1);
  } else if (p.type.indexOf('GRADIENT_') === 0) {
    var inv = ecInvert(p.gradientTransform);
    var linear = p.type === 'GRADIENT_LINEAR';
    out.gradientHandlePositions = linear
      ? [ecApply(inv, 0, 0.5), ecApply(inv, 1, 0.5), ecApply(inv, 0, 1)]
      : [ecApply(inv, 0.5, 0.5), ecApply(inv, 1, 0.5), ecApply(inv, 0.5, 1)];
    out.gradientStops = p.gradientStops.map(function (s) { return { position: ecRound(s.position), color: ecRgba(s.color) }; });
  } else if (p.type === 'IMAGE') {
    out.scaleMode = p.scaleMode;
  }
  return out;
}

function ecPaints(list) {
  if (!list || ecMixed(list)) return [];
  return list.map(ecPaint);
}

function ecHasImagePaint(n) {
  var lists = [n.fills, n.strokes];
  for (var i = 0; i < lists.length; i++) {
    var l = lists[i];
    if (!l || ecMixed(l)) continue;
    for (var j = 0; j < l.length; j++) if (l[j].visible !== false && EC_IMAGE_PAINTS[l[j].type]) return true;
  }
  return false;
}

function ecNeedsRaster(n) {
  if (!EC_NATIVE_TYPES[n.type]) return n.type + ' node';
  if (ecHasImagePaint(n)) return 'image fill';
  return null;
}

function ecBounds(b) { return b ? { x: ecRound(b.x), y: ecRound(b.y), width: ecRound(b.width), height: ecRound(b.height) } : undefined; }
function ecSameBounds(a, b) { return !a || !b || (a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height); }

function ecEffects(n) {
  if (!n.effects || ecMixed(n.effects)) return [];
  return n.effects.map(function (e) {
    var o = { type: e.type, radius: ecRound(e.radius) };
    if (e.visible === false) o.visible = false;
    if (e.color) o.color = ecRgba(e.color);
    if (e.offset) o.offset = { x: ecRound(e.offset.x), y: ecRound(e.offset.y) };
    if (e.spread) o.spread = ecRound(e.spread);
    if (e.blendMode && e.blendMode !== 'NORMAL') o.blendMode = e.blendMode;
    if (e.showShadowBehindNode === false) o.showShadowBehindNode = false;
    return o;
  });
}

function ecLineHeightPx(lh, size) {
  if (!lh || ecMixed(lh) || lh.unit === 'AUTO') return ecRound(size * 1.21);
  if (lh.unit === 'PERCENT') return ecRound(lh.value / 100 * size);
  return ecRound(lh.value);
}
function ecLetterSpacingPx(ls, size) {
  if (!ls || ecMixed(ls)) return 0;
  return ecRound(ls.unit === 'PERCENT' ? ls.value / 100 * size : ls.value);
}

function ecTypeStyle(seg) {
  var s = {
    fontFamily: seg.fontName.family,
    fontStyle: seg.fontName.style,
    fontSize: seg.fontSize,
    lineHeightPx: ecLineHeightPx(seg.lineHeight, seg.fontSize)
  };
  var ls = ecLetterSpacingPx(seg.letterSpacing, seg.fontSize);
  if (ls) s.letterSpacing = ls;
  if (seg.textCase && seg.textCase !== 'ORIGINAL') s.textCase = seg.textCase;
  if (seg.textDecoration && seg.textDecoration !== 'NONE') s.textDecoration = seg.textDecoration;
  if (seg.fills) s.fills = ecPaints(seg.fills);
  return s;
}

function ecText(n, out) {
  out.characters = n.characters;
  var segs = n.getStyledTextSegments(['fontName', 'fontSize', 'fills', 'letterSpacing', 'lineHeight', 'textCase', 'textDecoration']);
  var base = segs.length ? ecTypeStyle(segs[0]) : {};
  base.textAlignHorizontal = n.textAlignHorizontal;
  base.textAlignVertical = n.textAlignVertical;
  base.textAutoResize = n.textAutoResize;
  if (!ecMixed(n.paragraphSpacing) && n.paragraphSpacing) base.paragraphSpacing = n.paragraphSpacing;
  out.style = base;
  out.textAutoResize = n.textAutoResize;
  if (segs.length) out.fills = ecPaints(segs[0].fills);
  // Runs: one style id per UTF-16 code unit (JS string indices are UTF-16 already); trailing
  // zeros left out, as in the REST API.
  var overrides = [];
  var table = {};
  var key0 = JSON.stringify(ecTypeStyle(segs[0] || { fontName: {}, fontSize: 12 }));
  var ids = {};
  var next = 1;
  for (var i = 0; i < segs.length; i++) {
    var st = ecTypeStyle(segs[i]);
    var key = JSON.stringify(st);
    var id = 0;
    if (key !== key0) {
      if (!ids[key]) { ids[key] = next++; table[String(ids[key])] = st; }
      id = ids[key];
    }
    for (var k = segs[i].start; k < segs[i].end; k++) overrides[k] = id;
  }
  while (overrides.length && !overrides[overrides.length - 1]) overrides.pop();
  if (next > 1) { out.characterStyleOverrides = overrides; out.styleOverrideTable = table; }
}

// Path data with coordinates rounded to 1e-3 (Figma writes 15 digits).
function ecPath(d) {
  return String(d).replace(/-?\d*\.\d+(?:[eE][-+]?\d+)?/g, function (m) { return String(Math.round(parseFloat(m) * 1000) / 1000); });
}

function ecMatrix(m) { return m.map(function (row) { return row.map(ecRound); }); }

// The fields every node has (also what a node that comes in as an image keeps).
function ecBase(n) {
  var out = {
    id: n.id, name: n.name, type: n.type === 'POLYGON' ? 'REGULAR_POLYGON' : n.type,
    absoluteTransform: ecMatrix(n.absoluteTransform),
    size: { x: ecRound(n.width), y: ecRound(n.height) },
    absoluteBoundingBox: ecBounds(n.absoluteBoundingBox)
  };
  var rb = ecBounds(n.absoluteRenderBounds);
  if (!ecSameBounds(rb, out.absoluteBoundingBox)) out.absoluteRenderBounds = rb;
  if (n.visible === false) out.visible = false;
  if (n.opacity !== undefined && n.opacity !== 1) out.opacity = ecRound(n.opacity);
  if (n.blendMode && n.blendMode !== 'PASS_THROUGH' && n.blendMode !== 'NORMAL') out.blendMode = n.blendMode;
  if (n.isMask) { out.isMask = true; out.maskType = n.maskType || 'ALPHA'; }
  if ('effects' in n) { var fx = ecEffects(n); if (fx.length) out.effects = fx; }
  return out;
}

function ecNode(n, ctx) {
  ctx.count++;
  if (ctx.count > EC_MAX_NODES) return null;
  var out = ecBase(n);
  var raster = ecNeedsRaster(n);
  if (raster) {
    ctx.rasters.push({ node: n, reason: raster });
    // Keep a marker paint so EffectCraft knows this node arrives as an image.
    if (raster === 'image fill') out.fills = [{ type: 'IMAGE' }];
    return out;
  }
  if ('fills' in n) { var fills = ecPaints(n.fills); if (fills.length) out.fills = fills; }
  if ('strokes' in n) {
    var strokes = ecPaints(n.strokes);
    var w = n.strokeWeight;
    if (ecMixed(w)) w = Math.max(n.strokeTopWeight || 0, n.strokeRightWeight || 0, n.strokeBottomWeight || 0, n.strokeLeftWeight || 0);
    if (strokes.length && w > 0) {
      out.strokes = strokes;
      out.strokeWeight = w;
      out.strokeAlign = n.strokeAlign;
      out.strokeCap = ecMixed(n.strokeCap) ? 'NONE' : n.strokeCap;
      out.strokeJoin = ecMixed(n.strokeJoin) ? 'MITER' : n.strokeJoin;
      if (n.strokeMiterLimit) out.strokeMiterLimit = n.strokeMiterLimit;
      if (n.dashPattern && n.dashPattern.length) out.strokeDashes = n.dashPattern.slice();
    }
  }
  if (n.clipsContent) out.clipsContent = true;
  if ('cornerRadius' in n) {
    if (ecMixed(n.cornerRadius)) {
      if (typeof n.topLeftRadius === 'number') out.rectangleCornerRadii = [n.topLeftRadius, n.topRightRadius, n.bottomRightRadius, n.bottomLeftRadius];
    } else if (n.cornerRadius) {
      out.cornerRadius = n.cornerRadius;
    }
  }
  if (n.type === 'ELLIPSE') {
    var arc = n.arcData;
    if (arc && (arc.innerRadius || Math.abs(arc.endingAngle - arc.startingAngle - 2 * Math.PI) > 1e-4)) out.arcData = arc;
  }
  if (n.type === 'STAR' || n.type === 'POLYGON') {
    out.pointCount = n.pointCount;
    if (n.type === 'STAR') out.innerRadius = n.innerRadius;
  }
  if (n.type === 'VECTOR') {
    try { out.vectorPaths = n.vectorPaths.map(function (p) { return { path: ecPath(p.data), windingRule: p.windingRule }; }); } catch (e) { /* fall back to fillGeometry */ }
  }
  if ((n.type === 'VECTOR' && !out.vectorPaths) || n.type === 'STAR' || n.type === 'POLYGON' || n.type === 'BOOLEAN_OPERATION' || (n.type === 'ELLIPSE' && out.arcData)) {
    try { out.fillGeometry = n.fillGeometry.map(function (g) { return { path: ecPath(g.data), windingRule: g.windingRule }; }); } catch (e) { /* none */ }
  }
  if (n.type === 'TEXT') ecText(n, out);
  if ('children' in n && n.type !== 'BOOLEAN_OPERATION') {
    out.children = [];
    for (var i = 0; i < n.children.length; i++) {
      var c = ecNode(n.children[i], ctx);
      if (c) out.children.push(c);
    }
  }
  return out;
}

async function ecFind(ids, missing) {
  var found = [];
  for (var i = 0; i < ids.length; i++) {
    var id = String(ids[i]).trim();
    // Links use 12-345; the API uses 12:345 (instance sublayers: I12:345;6:7).
    if (/^\d+-\d+$/.test(id)) id = id.replace('-', ':');
    var n = await figma.getNodeByIdAsync(id);
    if (!n || n.type === 'DOCUMENT' || n.type === 'PAGE') { missing.push(ids[i]); continue; }
    found.push(n);
  }
  return found;
}

// Fit `reply` into `budget` characters: replace the largest subtrees with stubs (listed in
// `pending`); a node that can't be split and still doesn't fit becomes an image. Sizes are
// measured once, bottom-up.
function ecFit(reply, roots, budget, ctx) {
  if (!(budget > 0)) return;
  var total = JSON.stringify(reply).length;
  if (total <= budget) return;
  var recs = [];
  var measure = function (node, list, i, parent) {
    var rec = { node: node, list: list, i: i, parent: parent, size: 0, cut: false };
    var kids = node.children;
    if (kids) node.children = [];
    rec.size = JSON.stringify(node).length;
    if (kids) {
      node.children = kids;
      for (var k = 0; k < kids.length; k++) rec.size += measure(kids[k], kids, k, rec).size + 1;
    }
    if (parent) recs.push(rec);
    return rec;
  };
  var rootRecs = roots.map(function (n, i) { return measure(n, roots, i, null); });
  // Best fit: the smallest subtree whose stub brings the reply under budget, else the largest.
  var open = function (rec) {
    if (rec.cut || rec.size < 200) return false;
    for (var p = rec.parent; p; p = p.parent) if (p.cut) return false;
    return true;
  };
  for (var guard = 0; guard < 100000 && total > budget; guard++) {
    var excess = total - budget, fit = null, largest = null;
    for (var r = 0; r < recs.length; r++) {
      var c = recs[r];
      if (!open(c)) continue;
      if (!largest || c.size > largest.size) largest = c;
      if (c.size - 40 >= excess && (!fit || c.size < fit.size)) fit = c;
    }
    var rec = fit || largest;
    if (!rec) break;
    var stub = { $ref: rec.node.id };
    var delta = rec.size - JSON.stringify(stub).length;
    rec.list[rec.i] = stub;
    rec.cut = true;
    ctx.pending.push(rec.node.id);
    total -= delta;
    for (var q = rec.parent; q; q = q.parent) q.size -= delta;
  }
  // Still too big: the biggest root nodes come in as images.
  rootRecs.sort(function (a, b) { return b.size - a.size; });
  for (var k = 0; k < rootRecs.length && total > budget; k++) {
    var node = roots[rootRecs[k].i];
    var keep = { id: node.id, name: node.name, type: 'RECTANGLE', tooLarge: true, absoluteTransform: node.absoluteTransform, size: node.size, absoluteBoundingBox: node.absoluteBoundingBox, fills: [{ type: 'IMAGE' }] };
    if (node.visible === false) keep.visible = false;
    total -= rootRecs[k].size - JSON.stringify(keep).length;
    roots[rootRecs[k].i] = keep;
    ctx.rasterize.push({ id: node.id, name: node.name, reason: 'too large to transfer' });
  }
}

async function ecRender(nodes, opts, images) {
  var mode = opts.images || 'both';
  if (mode === 'none') return;
  for (var i = 0; i < nodes.length; i++) {
    var n = nodes[i];
    if (opts.onProgress) opts.onProgress('Rendering ' + (i + 1) + ' of ' + nodes.length + '…');
    var entry = {};
    if (mode === 'both' || mode === 'pdf') {
      try { entry.pdf = figma.base64Encode(await n.exportAsync({ format: 'PDF', useAbsoluteBounds: true })); } catch (e) { /* PNG next */ }
    }
    if (mode === 'both' || mode === 'png' || !entry.pdf) {
      try { entry.png = figma.base64Encode(await n.exportAsync({ format: 'PNG', constraint: { type: 'SCALE', value: opts.scale || 2 }, useAbsoluteBounds: true })); } catch (e) { /* noted by EffectCraft */ }
    }
    if (entry.pdf || entry.png) images[n.id] = entry;
  }
}

async function effectcraftSerialize(opts) {
  opts = opts || {};
  var missing = [];
  var images = {};
  if (opts.imagesOnly) {
    await ecRender(await ecFind(opts.imagesOnly, missing), opts, images);
    return { images: images, missing: missing };
  }
  var subtrees = opts.subtree ? [].concat(opts.subtree) : null;
  var sel = subtrees ? await ecFind(subtrees, missing) : opts.ids ? await ecFind(opts.ids, missing) : figma.currentPage.selection.slice();
  if (!sel.length) throw new Error(opts.ids || opts.subtree ? 'Figma has no node ' + missing.join(', ') : 'Select something to send.');
  var ctx = { count: 0, rasters: [], pending: [], rasterize: [] };
  var nodes = sel.map(function (n) { return ecNode(n, ctx); }).filter(Boolean);
  await ecRender(ctx.rasters.map(function (r) { return r.node; }), opts, images);
  var reply = { source: 'figma-plugin', fileKey: opts.fileKey || figma.fileKey || null };
  var deferred = [];
  if (subtrees && opts.budget) {
    // Several parts per reply: whole subtrees while they fit; the rest wait for the next call.
    var used = 0, kept = [];
    nodes.forEach(function (n) {
      var size = JSON.stringify(n).length;
      if (!kept.length || used + size <= opts.budget * 0.9) { kept.push(n); used += size; } else deferred.push(n.id);
    });
    nodes = kept;
  }
  if (subtrees) { reply.fragments = {}; nodes.forEach(function (n) { reply.fragments[n.id] = n; }); } else reply.nodes = nodes;
  reply.images = images;
  reply.rasterize = ctx.rasters.map(function (r) { return { id: r.node.id, name: r.node.name, reason: r.reason }; });
  reply.pending = ctx.pending.concat(deferred);
  reply.missing = missing;
  reply.stats = { nodes: ctx.count, truncated: ctx.count > EC_MAX_NODES };
  if (opts.budget) {
    // Stubs take nodes (and the images they need) out of this reply; their own parts list them.
    var roots = nodes.slice();
    ecFit(reply, roots, opts.budget, ctx);
    if (subtrees) { reply.fragments = {}; roots.forEach(function (n) { reply.fragments[n.id] = n; }); } else reply.nodes = roots;
    reply.pending = ctx.pending.concat(deferred);
    var inReply = JSON.stringify(roots);
    reply.rasterize = reply.rasterize.filter(function (r) { return inReply.indexOf('"id":' + JSON.stringify(r.id)) >= 0; }).concat(ctx.rasterize);
  }
  return reply;
}
