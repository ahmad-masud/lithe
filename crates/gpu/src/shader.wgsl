// One pipeline draws the entire UI.
//
// A glyph is a quad sampling its cell in the coverage atlas. A filled rect is
// a quad sampling the atlas's reserved opaque texel, so it comes out as flat
// colour. Same instance layout, same pipeline, one draw call per frame.
//
// No vertex buffer is bound for geometry: the unit quad is generated from
// @builtin(vertex_index), so the only per-frame upload is the instance array.

struct Globals {
    // Surface size in physical pixels, for the px -> clip-space map.
    screen: vec2<f32>,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_smp: sampler;

struct Quad {
    @location(0) pos: vec2<f32>,    // top-left, physical px
    @location(1) size: vec2<f32>,   // physical px
    @location(2) uv: vec4<f32>,     // xy = uv min, zw = uv max
    @location(3) color: vec4<f32>,
    // 0 samples coverage from the glyph atlas; 1 samples full colour, for
    // an image; 2 is a rounded rectangle, whose coverage the fragment
    // shader works out rather than sampling. The bound texture differs
    // for an image too, but the pipeline does not.
    @location(4) mode: f32,
    // Corner radius in physical px, for mode 2.
    @location(5) radius: f32,
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) mode: f32,
    // Where this fragment is inside the quad, measured from its
    // centre, and the quad's own half-size and radius. A rounded
    // corner is a distance field, and a distance field needs to know
    // where it is.
    @location(3) local: vec2<f32>,
    @location(4) @interpolate(flat) half_size: vec2<f32>,
    @location(5) @interpolate(flat) radius: f32,
}

@vertex
fn vs(q: Quad, @builtin(vertex_index) vi: u32) -> VsOut {
    var corners: array<vec2<f32>, 6> = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vi];
    let px = q.pos + corner * q.size;

    var out: VsOut;
    // Origin is top-left, so y is flipped on the way to NDC.
    out.clip = vec4<f32>(
        (px.x / globals.screen.x) * 2.0 - 1.0,
        1.0 - (px.y / globals.screen.y) * 2.0,
        0.0,
        1.0,
    );
    out.uv = mix(q.uv.xy, q.uv.zw, corner);
    out.color = q.color;
    out.mode = q.mode;
    out.local = (corner - 0.5) * q.size;
    out.half_size = q.size * 0.5;
    out.radius = q.radius;
    return out;
}

// Signed distance to a rounded box centred on the origin. Negative
// inside, positive outside — the standard formulation, kept because
// anything cleverer is harder to check.
fn rounded_box(p: vec2<f32>, half_size: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half_size + vec2<f32>(r, r);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    if (in.mode > 1.5) {
        // A rounded rectangle. One pixel of feather at the edge, which
        // is what makes a corner read as a curve rather than as a
        // staircase.
        let d = rounded_box(in.local, in.half_size, in.radius);
        let coverage = 1.0 - smoothstep(-0.5, 0.5, d);
        return vec4<f32>(in.color.rgb, in.color.a * coverage);
    }
    let texel = textureSample(atlas_tex, atlas_smp, in.uv);
    if (in.mode > 0.5) {
        // An image: the bound texture already holds the colour.
        return vec4<f32>(texel.rgb, texel.a * in.color.a);
    }
    // The glyph atlas is single-channel coverage. Rects sample the
    // reserved opaque texel and so land on flat colour for free.
    return vec4<f32>(in.color.rgb, in.color.a * texel.r);
}
