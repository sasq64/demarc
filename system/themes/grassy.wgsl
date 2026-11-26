const BLADES = 110;

var<private> frag_coord: vec4f;

fn rotateX(a: f32, v: vec3f) -> vec3f {
    return vec3f(v.x, cos(a) * v.y + sin(a) * v.z, cos(a) * v.z - sin(a) * v.y);
}

fn rotateY(a: f32, v: vec3f) -> vec3f {
    return vec3f(cos(a) * v.x + sin(a) * v.z, v.y, cos(a) * v.z - sin(a) * v.x);
}

fn rotateZ(a: f32, v: vec3f) -> vec3f {
    return vec3f(cos(a) * v.x + sin(a) * v.y, cos(a) * v.y - sin(a) * v.x, v.z);
}

fn grass(p_in: vec2f, x: f32) -> vec4f {
    var p = p_in;
    let s = mix(0.7, 2.0, 0.5 + sin(x * 12.0) * 0.5);
    p.x += pow(1.0 + p.y, 2.0) * 0.1 * cos(x * 0.5 + ui.time);
    p.x *= s;
    p.y = (1.0 + p.y) * s - 1.0;
    let m = 1.0 - smoothstep(0.0, clamp(1.0 - p.y * 1.5, 0.01, 0.6) * 0.2 * s, pow(abs(p.x) * 19.0, 1.5) + p.y - 0.6);
    return vec4f(mix(vec3f(0.05, 0.1, 0.0) * 0.8, vec3f(0.0, 0.3, 0.0), (p.y + 1.0) * 0.5 + abs(p.x)), m * smoothstep(-1.0, -0.9, p.y));
}

fn backg(ro: vec3f, rd: vec3f) -> vec3f {
    let t = (-1.0 - ro.y) / rd.y;
    let tc = ro.xz + rd.xz * t;
    let horiz = vec3f(0.0, 0.2, 0.2) * 0.7;
    let sky = mix(horiz, vec3f(0.1, 0.13, 0.15) * 0.8, dot(rd, vec3f(0.0, 1.0, 0.0)));
    let ground = mix(horiz, vec3f(0.04, 0.07, 0.0) * 0.6, pow(max(0.0, dot(rd, vec3f(0.0, -1.0, 0.0))), 0.2));
    return mix(sky, ground, step(0.0, t));
}

fn dither() -> f32 {
    return fract(frag_coord.x * 0.482635532 + frag_coord.y * 0.1353412 + ui.time * 100.0) * 0.008;
}

fn mainImage(fragCoord: vec2f) -> vec4f {
    var fragColor = vec4f(0.0);
    let ct = vec3f(0.0, 1.0, 5.0);
    let cp = rotateY(cos(ui.time * 0.2) * 0.4, vec3f(0.0, 0.6, 0.0));
    let cw = normalize(cp - ct);
    let cu = normalize(cross(cw, vec3f(0.0, 1.0, 0.0)));
    let cv = normalize(cross(cu, cw));
    let rm = mat3x3f(cu, cv, cw);
    let uv = fragCoord.xy / ui.resolution * 2.0 - vec2f(1.0);
    var t = uv;
    t.x *= ui.resolution.x / ui.resolution.y;
    let ro = cp;
    let rd = rotateY(sin(ui.time * 0.7) * 0.1, rm * rotateZ(sin(ui.time * 0.15) * 0.1, vec3f(t, -1.3)));
    var fcol = backg(ro, rd);
    for (var i = 0; i < BLADES; i += 1) {
        let z = -(f32(BLADES - i) * 0.1 + 1.0);
        let pln = vec4f(0.0, 0.0, -1.0, z);
        var t = (pln.w - dot(pln.xyz, ro)) / dot(pln.xyz, rd);
        var tc = ro.xy + rd.xy * t;
        tc.x += cos(f32(i) * 3.0) * 4.0;
        let cell = floor(tc.x);
        tc.x = tc.x - cell - 0.5;
        let c = grass(tc, f32(i) + cell * 10.0);
        fcol = mix(fcol, c.rgb, step(0.0, t) * c.w);
    }
    fcol = pow(fcol * 1.1, vec3f(0.8));
    let q = (uv + vec2f(1.0)) * 0.5;
    fcol *= 0.2 + 0.8 * pow(16.0 * q.x * q.y * (1.0 - q.x) * (1.0 - q.y), 0.1);
    let fragColor_xyz = fcol * 1.8 + vec3f(dither());
    fragColor.x = fragColor_xyz.x;
    fragColor.y = fragColor_xyz.y;
    fragColor.z = fragColor_xyz.z;
    fragColor.a = 1.0;
    return fragColor;
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4f {
    let coord = vec2f(in.uv.x, 1.0 - in.uv.y) * ui.resolution;
    frag_coord = vec4f(coord, 0.0, 1.0);
    return vec4f(mainImage(coord).rgb, 1.0);
}
