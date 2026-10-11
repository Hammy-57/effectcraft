// Packed 8/16 bpc layer codes to their floats: each channel `k` is `k * (1 / levels)`, rounded
// as on the CPU (`ieee_mul_bits`), bit-identical to the f32 upload (`GpuContext::upload_codes`).
@group(0) @binding(8) var codes: texture_2d<u32>;
@group(0) @binding(9) var<uniform> unpack_scale: vec4<u32>;
@compute @workgroup_size(16, 16)
fn unpack(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(out)) { return; }
    let pos = vec2<i32>(id.xy);
    let k = textureLoad(codes, pos, 0);
    let s = unpack_scale.x;
    let v = vec4<u32>(ieee_mul_bits(bitcast<u32>(f32(k.x)), s), ieee_mul_bits(bitcast<u32>(f32(k.y)), s), ieee_mul_bits(bitcast<u32>(f32(k.z)), s), ieee_mul_bits(bitcast<u32>(f32(k.w)), s));
    textureStore(out, pos, bitcast<vec4<f32>>(v));
}
