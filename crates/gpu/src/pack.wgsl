// A retained 8 bpc image to its integer codes (`Enc::keep_resident`): each channel of an image
// on the 8 bpc grid (`k * (1 / 255)`, see `ops::quantize`) back to `k`, a quarter of the bytes.
// `unpack` restores the identical floats.
@group(0) @binding(0) var pack_src: texture_2d<f32>;
@group(0) @binding(1) var pack_out: texture_storage_2d<rgba8uint, write>;
@compute @workgroup_size(16, 16)
fn pack8(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(pack_out)) { return; }
    let pos = vec2<i32>(id.xy);
    let v = clamp(textureLoad(pack_src, pos, 0), vec4<f32>(0.0), vec4<f32>(1.0));
    textureStore(pack_out, pos, vec4<u32>(round(v * 255.0)));
}
