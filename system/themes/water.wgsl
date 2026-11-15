const DRAG_MULT = 0.38;
const WATER_DEPTH = 1.0;
const CAMERA_HEIGHT = 1.5;
const ITERATIONS_RAYMARCH = 12;
const ITERATIONS_NORMAL = 36;

fn wavedx(position: vec2f, direction: vec2f, frequency: f32, timeshift: f32) -> vec2f {
    let x = dot(direction, position) * frequency + timeshift;
    let wave = exp(sin(x) - 1.0);
    let dx = wave * cos(x);
    return vec2f(wave, -dx);
}

fn getwaves(position_in: vec2f, iterations: i32) -> f32 {
    var position = position_in;
    let wavePhaseShift = length(position) * 0.1;
    var iter = 0.0;
    var frequency = 1.0;
    var timeMultiplier = 2.0;
    var weight = 1.0;
    var sumOfValues = 0.0;
    var sumOfWeights = 0.0;
    for (var i = 0; i < iterations; i++) {
        let p = vec2f(sin(iter), cos(iter));
        let res = wavedx(position, p, frequency, ui.time * timeMultiplier + wavePhaseShift);
        position += p * res.y * weight * DRAG_MULT;
        sumOfValues += res.x * weight;
        sumOfWeights += weight;
        weight = mix(weight, 0.0, 0.2);
        frequency *= 1.18;
        timeMultiplier *= 1.07;
        iter += 1232.399963;
    }
    return sumOfValues / sumOfWeights;
}

fn raymarchwater(camera: vec3f, start: vec3f, end: vec3f, depth: f32) -> f32 {
    var pos = start;
    let dir = normalize(end - start);
    for (var i = 0; i < 64; i++) {
        let height = getwaves(pos.xz, ITERATIONS_RAYMARCH) * depth - depth;
        if height + 0.01 > pos.y {
            return distance(pos, camera);
        }
        pos += dir * (pos.y - height);
    }
    return distance(start, camera);
}

fn normal(pos: vec2f, e: f32, depth: f32) -> vec3f {
    let ex = vec2f(e, 0.0);
    let H = getwaves(pos.xy, ITERATIONS_NORMAL) * depth;
    let a = vec3f(pos.x, H, pos.y);
    return normalize(cross(a - vec3f(pos.x - e, getwaves(pos.xy - ex.xy, ITERATIONS_NORMAL) * depth, pos.y), a - vec3f(pos.x, getwaves(pos.xy + ex.yx, ITERATIONS_NORMAL) * depth, pos.y + e)));
}

fn createRotationMatrixAxisAngle(axis: vec3f, angle: f32) -> mat3x3f {
    let s = sin(angle);
    let c = cos(angle);
    let oc = 1.0 - c;
    return mat3x3f(oc * axis.x * axis.x + c, oc * axis.x * axis.y - axis.z * s, oc * axis.z * axis.x + axis.y * s, oc * axis.x * axis.y + axis.z * s, oc * axis.y * axis.y + c, oc * axis.y * axis.z - axis.x * s, oc * axis.z * axis.x - axis.y * s, oc * axis.y * axis.z + axis.x * s, oc * axis.z * axis.z + c);
}

fn getRay(fragCoord: vec2f) -> vec3f {
    let uv = (fragCoord.xy / ui.resolution * 2.0 - 1.0) * vec2f(ui.resolution.x / ui.resolution.y, 1.0);
    let proj = normalize(vec3f(uv.x, uv.y, 1.5));
    if ui.resolution.x < 600.0 {
        return proj;
    }
    return createRotationMatrixAxisAngle(vec3f(0.0, -1.0, 0.0), 3.0 * (((vec2f(0.0) / ui.resolution).x + 0.5) * 2.0 - 1.0)) * createRotationMatrixAxisAngle(vec3f(1.0, 0.0, 0.0), 0.5 + 1.5 * (select((vec2f(0.0) / ui.resolution).y, 0.27, (vec2f(0.0) / ui.resolution).y == 0.0) * 1.0 * 2.0 - 1.0)) * proj;
}

fn intersectPlane(origin: vec3f, direction: vec3f, point: vec3f, normal: vec3f) -> f32 {
    return clamp(dot(point - origin, normal) / dot(direction, normal), -1.0, 9991999.0);
}

fn extra_cheap_atmosphere(raydir: vec3f, sundir: vec3f) -> vec3f {
    let special_trick = 1.0 / (raydir.y * 1.0 + 0.1);
    let special_trick2 = 1.0 / (sundir.y * 11.0 + 1.0);
    let raysundt = pow(abs(dot(sundir, raydir)), 2.0);
    let sundt = pow(max(0.0, dot(sundir, raydir)), 8.0);
    let mymie = sundt * special_trick * 0.2;
    let suncolor = mix(vec3f(1.0), max(vec3f(0.0), vec3f(1.0) - vec3f(5.5, 13.0, 22.4) / 22.4), special_trick2);
    let bluesky = vec3f(5.5, 13.0, 22.4) / 22.4 * suncolor;
    var bluesky2 = max(vec3f(0.0), bluesky - vec3f(5.5, 13.0, 22.4) * 0.002 * (special_trick + -6.0 * sundir.y * sundir.y));
    bluesky2 *= special_trick * (0.24 + raysundt * 0.24);
    return bluesky2 * (1.0 + 1.0 * pow(1.0 - raydir.y, 3.0));
}

fn getSunDirection() -> vec3f {
    return normalize(vec3f(-0.0773502691896258, 0.5 + sin(ui.time * 0.2 + 2.6) * 0.45, 0.5773502691896258));
}

fn getAtmosphere(dir: vec3f) -> vec3f {
    return extra_cheap_atmosphere(dir, getSunDirection()) * 0.5;
}

fn getSun(dir: vec3f) -> f32 {
    return pow(max(0.0, dot(dir, getSunDirection())), 720.0) * 210.0;
}

fn aces_tonemap(color: vec3f) -> vec3f {
    let m1 = mat3x3f(0.59719, 0.076, 0.0284, 0.35458, 0.90834, 0.13383, 0.04823, 0.01566, 0.83777);
    let m2 = mat3x3f(1.60475, -0.10208, -0.00327, -0.53108, 1.10813, -0.07276, -0.07367, -0.00605, 1.07602);
    let v = m1 * color;
    let a = v * (v + 0.0245786) - 0.000090537;
    let b = v * (0.983729 * v + 0.432951) + 0.238081;
    return pow(clamp(m2 * (a / b), vec3f(0.0), vec3f(1.0)), vec3f(1.0 / 2.2));
}

fn mainImage(fragCoord: vec2f) -> vec4f {
    var fragColor = vec4f(0.0);
    let ray = getRay(fragCoord);
    if ray.y >= 0.0 {
        let C = getAtmosphere(ray) + getSun(ray);
        fragColor = vec4f(aces_tonemap(C * 2.0), 1.0);
        return fragColor;
    }
    let waterPlaneHigh = vec3f(0.0, 0.0, 0.0);
    let waterPlaneLow = vec3f(0.0, -WATER_DEPTH, 0.0);
    let origin = vec3f(ui.time * 0.2, CAMERA_HEIGHT, 1.0);
    let highPlaneHit = intersectPlane(origin, ray, waterPlaneHigh, vec3f(0.0, 1.0, 0.0));
    let lowPlaneHit = intersectPlane(origin, ray, waterPlaneLow, vec3f(0.0, 1.0, 0.0));
    let highHitPos = origin + ray * highPlaneHit;
    let lowHitPos = origin + ray * lowPlaneHit;
    let dist = raymarchwater(origin, highHitPos, lowHitPos, WATER_DEPTH);
    let waterHitPos = origin + ray * dist;
    var N = normal(waterHitPos.xz, 0.01, WATER_DEPTH);
    N = mix(N, vec3f(0.0, 1.0, 0.0), 0.8 * min(1.0, sqrt(dist * 0.01) * 1.1));
    let fresnel = 0.04 + (1.0 - 0.04) * pow(1.0 - max(0.0, dot(-N, ray)), 5.0);
    var R = normalize(reflect(ray, N));
    R.y = abs(R.y);
    let reflection = getAtmosphere(R) + getSun(R);
    let scattering = vec3f(0.0293, 0.0698, 0.1717) * 0.1 * (0.2 + (waterHitPos.y + WATER_DEPTH) / WATER_DEPTH);
    let C = fresnel * reflection + scattering;
    fragColor = vec4f(aces_tonemap(C * 2.0), 1.0);
    return fragColor;
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4f {
    let coord = vec2f(in.uv.x, 1.0 - in.uv.y) * ui.resolution;
    return vec4f(mainImage(coord).rgb, 1.0);
}
