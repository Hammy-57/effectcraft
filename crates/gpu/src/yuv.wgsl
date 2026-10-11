struct Params { shape: vec4<u32>, range: vec4<f32>, rgb: vec4<f32>, alpha: vec4<f32> }
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var y_plane: texture_2d<u32>;
@group(0) @binding(2) var u_plane: texture_2d<u32>;
@group(0) @binding(3) var v_plane: texture_2d<u32>;
@group(0) @binding(4) var a_plane: texture_2d<u32>;
@group(0) @binding(5) var out: texture_storage_2d<rgba32float, write>;
@group(0) @binding(6) var rgba: texture_2d<f32>;
@group(0) @binding(7) var coverage: texture_2d<f32>;

fn vertical(t: texture_2d<u32>, x: i32, y: i32, other: i32, weight: f32) -> u32 {
    let a = bitcast<u32>(f32(textureLoad(t, vec2<i32>(x, y), 0).r));
    if p.shape.y == 0u { return ieee_mul_bits(ieee_sub_bits(a,bitcast<u32>(p.range.z)),bitcast<u32>(p.range.w)); }
    let b = bitcast<u32>(f32(textureLoad(t, vec2<i32>(x, other), 0).r));
    let first=ieee_mul_bits(a,bitcast<u32>(1.0-weight));
    let second=ieee_mul_bits(b,bitcast<u32>(weight));
    let interpolated=ieee_add_bits(first,second);
    return ieee_mul_bits(ieee_sub_bits(interpolated,bitcast<u32>(p.range.z)),bitcast<u32>(p.range.w));
}
fn chroma(t: texture_2d<u32>, pos: vec2<u32>) -> u32 {
    let dims = vec2<i32>(textureDimensions(t));
    let center = min(vec2<i32>(pos >> p.shape.xy), dims - 1);
    let neighbor = clamp(center + vec2<i32>(select(-1, 1, pos.x % 2u == 1u), select(-1, 1, pos.y % 2u == 1u)), vec2<i32>(0), dims - 1);
    let weight = select(0.0, 0.25, p.shape.y == 1u);
    let a = vertical(t, center.x, center.y, neighbor.y, weight);
    if p.shape.x == 0u { return a; }
    let b = vertical(t, neighbor.x, center.y, neighbor.y, weight);
    return ieee_add_bits(ieee_mul_bits(a,bitcast<u32>(0.75)),ieee_mul_bits(b,bitcast<u32>(0.25)));
}
@compute @workgroup_size(16, 16)
fn decode(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(out)) { return; }
    let pos = vec2<i32>(id.xy);
    let y = ieee_mul_bits(ieee_sub_bits(bitcast<u32>(f32(textureLoad(y_plane, pos, 0).r)), bitcast<u32>(p.range.x)), bitcast<u32>(p.range.y));
    let u = chroma(u_plane, id.xy);
    let v = chroma(v_plane, id.xy);
    let red=ieee_add_bits(y,ieee_mul_bits(bitcast<u32>(p.rgb.x),v));
    let green=ieee_sub_bits(ieee_sub_bits(y,ieee_mul_bits(bitcast<u32>(p.rgb.y),v)),ieee_mul_bits(bitcast<u32>(p.rgb.z),u));
    let blue=ieee_add_bits(y,ieee_mul_bits(bitcast<u32>(p.rgb.w),u));
    var rgb = clamp(bitcast<vec3<f32>>(vec3<u32>(red,green,blue)), vec3<f32>(0.0), vec3<f32>(1.0));
    var a = 1.0;
    if p.shape.z != 0u && p.shape.w != 0u {
        a = clamp(f32(textureLoad(a_plane, pos, 0).r) * p.alpha.w, 0.0, 1.0);
        if p.shape.w == 1u { rgb *= a; }
        else { rgb = max(rgb - p.alpha.xyz * (1.0 - a), vec3<f32>(0.0)); }
    }
    textureStore(out, pos, vec4<f32>(rgb, a));
}
@compute @workgroup_size(16, 16)
fn mask(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(out)) { return; }
    let pos = vec2<i32>(id.xy);
    textureStore(out, pos, textureLoad(rgba, pos, 0) * textureLoad(coverage, pos, 0).r);
}
