struct Ui {
    resolution: vec2<f32>,
    origin: vec2<f32>,
    screen: vec2<f32>,
    time: f32,
    _pad: f32,
}

struct FragmentOutput {
    @location(0) st_color: vec4<f32>,
}

@group(0) @binding(0) 
var<uniform> ui: Ui;
@group(0) @binding(1) 
var tex: texture_2d<f32>;
@group(0) @binding(2) 
var samp: sampler;
var<private> st_uv_1: vec2<f32>;
var<private> st_color: vec4<f32>;
var<private> st_frag_coord: vec4<f32>;

fn sdCapsule(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>, r: f32) -> f32 {
    var p_1: vec3<f32>;
    var a_1: vec3<f32>;
    var b_1: vec3<f32>;
    var r_1: f32;
    var pa: vec3<f32>;
    var ba: vec3<f32>;
    var h: f32;

    p_1 = p;
    a_1 = a;
    b_1 = b;
    r_1 = r;
    let _e24 = p_1;
    let _e25 = a_1;
    pa = (_e24 - _e25);
    let _e28 = b_1;
    let _e29 = a_1;
    ba = (_e28 - _e29);
    let _e32 = pa;
    let _e33 = ba;
    let _e35 = ba;
    let _e36 = ba;
    h = clamp((dot(_e32, _e33) / dot(_e35, _e36)), 0f, 1f);
    let _e43 = pa;
    let _e44 = ba;
    let _e45 = h;
    let _e49 = r_1;
    return (length((_e43 - (_e44 * _e45))) - _e49);
}

fn map(pos: vec3<f32>) -> vec2<f32> {
    var pos_1: vec3<f32>;
    var res: vec2<f32> = vec2<f32>(10000000000f, 0f);
    var i: f32 = 0f;
    var localTime: f32;
    var local: vec2<f32>;

    pos_1 = pos;
    loop {
        let _e24 = i;
        if !((_e24 < 6f)) {
            break;
        }
        {
            let _e31 = ui;
            let _e34 = i;
            localTime = (_e31.time - ((3.14159f * _e34) / 5f));
            let _e40 = localTime;
            localTime = (_e40 * 1.2f);
            let _e43 = res;
            let _e45 = pos_1;
            let _e48 = localTime;
            let _e52 = i;
            let _e75 = sdCapsule((_e45 - vec3<f32>(0f, (2f * cos(_e48)), ((2f * _e52) * 0.5f))), vec3<f32>(-10f, 0f, 0f), vec3<f32>(10f, 0f, 0f), 0.5f);
            let _e76 = i;
            if (_e43.x < vec2<f32>(_e75, _e76).x) {
                let _e80 = res;
                local = _e80;
            } else {
                let _e81 = pos_1;
                let _e84 = localTime;
                let _e88 = i;
                let _e111 = sdCapsule((_e81 - vec3<f32>(0f, (2f * cos(_e84)), ((2f * _e88) * 0.5f))), vec3<f32>(-10f, 0f, 0f), vec3<f32>(10f, 0f, 0f), 0.5f);
                let _e112 = i;
                local = vec2<f32>(_e111, _e112);
            }
            let _e115 = local;
            res = _e115;
        }
        continuing {
            let _e28 = i;
            i = (_e28 + 1f);
        }
    }
    let _e116 = res;
    return _e116;
}

fn castRay(ro: vec3<f32>, rd: vec3<f32>) -> vec2<f32> {
    var ro_1: vec3<f32>;
    var rd_1: vec3<f32>;
    var res_1: vec2<f32> = vec2<f32>(-1f, -1f);
    var tmin: f32 = 1f;
    var t: f32;
    var i_1: i32 = 0i;
    var h_1: vec2<f32>;

    ro_1 = ro;
    rd_1 = rd;
    let _e28 = tmin;
    t = _e28;
    loop {
        let _e32 = i_1;
        if !((_e32 < 150i)) {
            break;
        }
        {
            let _e39 = ro_1;
            let _e40 = rd_1;
            let _e41 = t;
            let _e44 = map((_e39 + (_e40 * _e41)));
            h_1 = _e44;
            let _e46 = h_1;
            let _e50 = t;
            if (abs(_e46.x) < (0.0001f * _e50)) {
                {
                    let _e53 = t;
                    let _e54 = h_1;
                    res_1 = vec2<f32>(_e53, _e54.y);
                    break;
                }
            }
            let _e57 = t;
            let _e58 = h_1;
            t = (_e57 + _e58.x);
        }
        continuing {
            let _e36 = i_1;
            i_1 = (_e36 + 1i);
        }
    }
    let _e61 = res_1;
    return _e61;
}

fn calcNormal(pos_2: vec3<f32>) -> vec3<f32> {
    var pos_3: vec3<f32>;
    var n: vec3<f32> = vec3(0f);
    var i_2: i32 = 0i;
    var e: vec3<f32>;

    pos_3 = pos_2;
    loop {
        let _e23 = i_2;
        if !((_e23 < 4i)) {
            break;
        }
        {
            let _e32 = i_2;
            let _e40 = i_2;
            let _e46 = i_2;
            e = (0.5773f * ((2f * vec3<f32>(f32((((_e32 + 3i) >> 1u) & 1i)), f32(((_e40 >> 1u) & 1i)), f32((_e46 & 1i)))) - vec3(1f)));
            let _e59 = n;
            let _e60 = e;
            let _e61 = pos_3;
            let _e63 = e;
            let _e66 = map((_e61 + (0.0005f * _e63)));
            n = (_e59 + (_e60 * _e66.x));
        }
        continuing {
            let _e27 = i_2;
            i_2 = (_e27 + 1i);
        }
    }
    let _e70 = n;
    return normalize(_e70);
}

fn rainbow_lookup(c: i32) -> vec3<f32> {
    var c_1: i32;
    var col: vec3<f32> = vec3(0f);

    c_1 = c;
    let _e21 = c_1;
    if (_e21 == 0i) {
        {
            col = vec3<f32>(1f, 0f, 0f);
        }
    } else {
        let _e28 = c_1;
        if (_e28 == 1i) {
            {
                col = vec3<f32>(1f, 0.5f, 0f);
            }
        } else {
            let _e35 = c_1;
            if (_e35 == 2i) {
                {
                    col = vec3<f32>(1f, 0.9f, 0f);
                }
            } else {
                let _e42 = c_1;
                if (_e42 == 3i) {
                    {
                        col = vec3<f32>(0f, 0.9f, 0f);
                    }
                } else {
                    let _e49 = c_1;
                    if (_e49 == 4i) {
                        {
                            col = vec3<f32>(0f, 0f, 1f);
                        }
                    } else {
                        let _e56 = c_1;
                        if (_e56 == 5i) {
                            {
                                col = vec3<f32>(0.8f, 0f, 0.8f);
                            }
                        }
                    }
                }
            }
        }
    }
    let _e63 = col;
    return _e63;
}

fn render(ro_2: vec3<f32>, rd_2: vec3<f32>) -> vec3<f32> {
    var ro_3: vec3<f32>;
    var rd_3: vec3<f32>;
    var res_2: vec2<f32>;
    var pos_4: vec3<f32>;
    var nor: vec3<f32>;
    var ref_: vec3<f32>;
    var col_1: vec3<f32>;
    var lig: vec3<f32> = vec3<f32>(0f, 0.11914522f, -0.9928768f);
    var hal: vec3<f32>;
    var amb: f32;
    var dif: f32;
    var bac: f32;
    var spe: f32;
    var lin: vec3<f32> = vec3(0f);

    ro_3 = ro_2;
    rd_3 = rd_2;
    let _e20 = ro_3;
    let _e21 = rd_3;
    let _e22 = castRay(_e20, _e21);
    res_2 = _e22;
    let _e24 = ro_3;
    let _e25 = res_2;
    let _e27 = rd_3;
    pos_4 = (_e24 + (_e25.x * _e27));
    let _e31 = pos_4;
    let _e32 = calcNormal(_e31);
    nor = _e32;
    let _e34 = rd_3;
    let _e35 = nor;
    ref_ = reflect(_e34, _e35);
    let _e38 = res_2;
    let _e41 = rainbow_lookup(i32(_e38.y));
    col_1 = _e41;
    let _e56 = lig;
    let _e57 = rd_3;
    hal = normalize((_e56 - _e57));
    let _e63 = nor;
    amb = sqrt(clamp((0.5f + (0.5f * _e63.y)), 0f, 1f));
    let _e72 = nor;
    let _e73 = lig;
    dif = clamp(dot(_e72, _e73), 0f, 1f);
    let _e79 = nor;
    let _e80 = lig;
    let _e84 = lig;
    let _e94 = pos_4;
    bac = (clamp(dot(_e79, normalize(vec3<f32>(-(_e80.x), 0f, -(_e84.z)))), 0f, 1f) * clamp((1f - _e94.y), 0f, 1f));
    let _e102 = nor;
    let _e103 = hal;
    spe = pow(clamp(dot(_e102, _e103), 0f, 1f), 50f);
    let _e114 = lin;
    let _e116 = amb;
    lin = (_e114 + ((0.55f * _e116) * vec3<f32>(0.6f, 0.6f, 0.8f)));
    let _e124 = lin;
    let _e126 = bac;
    lin = (_e124 + ((0.55f * _e126) * vec3<f32>(0.25f, 0.25f, 0.25f)));
    let _e134 = col_1;
    let _e135 = lin;
    col_1 = (_e134 * _e135);
    let _e137 = col_1;
    let _e139 = spe;
    col_1 = (_e137 + ((1f * _e139) * vec3<f32>(0.8f, 0.7f, 0.5f)));
    let _e147 = col_1;
    return vec3<f32>(clamp(_e147, vec3(0f), vec3(1f)));
}

fn setCamera(ro_4: vec3<f32>, ta: vec3<f32>, cr: f32) -> mat3x3<f32> {
    var ro_5: vec3<f32>;
    var ta_1: vec3<f32>;
    var cr_1: f32;
    var cw: vec3<f32>;
    var cp: vec3<f32>;
    var cu: vec3<f32>;
    var cv: vec3<f32>;

    ro_5 = ro_4;
    ta_1 = ta;
    cr_1 = cr;
    let _e22 = ta_1;
    let _e23 = ro_5;
    cw = normalize((_e22 - _e23));
    let _e27 = cr_1;
    let _e29 = cr_1;
    cp = vec3<f32>(sin(_e27), cos(_e29), 0f);
    let _e34 = cw;
    let _e35 = cp;
    cu = normalize(cross(_e34, _e35));
    let _e39 = cu;
    let _e40 = cw;
    cv = normalize(cross(_e39, _e40));
    let _e44 = cu;
    let _e45 = cv;
    let _e46 = cw;
    return mat3x3<f32>(vec3<f32>(_e44.x, _e44.y, _e44.z), vec3<f32>(_e45.x, _e45.y, _e45.z), vec3<f32>(_e46.x, _e46.y, _e46.z));
}

fn mainImage(fragColor: ptr<function, vec4<f32>>, fragCoord: vec2<f32>) {
    var fragCoord_1: vec2<f32>;
    var time: f32;
    var p_2: vec2<f32>;
    var ro_6: vec3<f32>;
    var ta_2: vec3<f32> = vec3<f32>(0f, 0f, 0f);
    var camMat: mat3x3<f32>;
    var rd_4: vec3<f32>;
    var col_2: vec3<f32>;

    fragCoord_1 = fragCoord;
    let _e19 = ui;
    let _e22 = (_e19.time / 2f);
    time = ((_e22 - (floor((_e22 / 9.28318f)) * 9.28318f)) - 3f);
    let _e35 = time;
    if (_e35 < 0f) {
        {
            time = 0f;
        }
    }
    let _e40 = st_frag_coord;
    let _e43 = ui;
    let _e51 = ui;
    p_2 = (((2f * _e40.xy) - vec3<f32>(_e43.resolution.x, _e43.resolution.y, 1f).xy) / vec2(vec3<f32>(_e51.resolution.x, _e51.resolution.y, 1f).y));
    let _e62 = time;
    let _e68 = time;
    ro_6 = vec3<f32>((20f * sin(_e62)), 0f, (-20f * cos(_e68)));
    let _e78 = ro_6;
    let _e79 = ta_2;
    let _e81 = setCamera(_e78, _e79, 0f);
    camMat = _e81;
    let _e83 = camMat;
    let _e84 = p_2;
    rd_4 = normalize((_e83 * vec3<f32>(_e84.x, _e84.y, 8f)));
    let _e92 = ro_6;
    let _e93 = rd_4;
    let _e94 = render(_e92, _e93);
    col_2 = _e94;
    let _e96 = col_2;
    col_2 = pow(_e96, vec3(0.7f));
    let _e100 = col_2;
    (*fragColor) = vec4<f32>(_e100.x, _e100.y, _e100.z, 1f);
    return;
}

fn main() {
    var color: vec4<f32> = vec4(0f);

    let _e16 = st_uv_1;
    let _e19 = st_uv_1;
    let _e23 = ui;
    let _e25 = (vec2<f32>(_e16.x, (1f - _e19.y)) * _e23.resolution);
    st_frag_coord = vec4<f32>(_e25.x, _e25.y, 0f, 1f);
    let _e35 = st_frag_coord;
    mainImage((&color), _e35.xy);
    let _e38 = color;
    let _e39 = _e38.xyz;
    st_color = vec4<f32>(_e39.x, _e39.y, _e39.z, 1f);
    return;
}

@fragment 
fn fragment(@location(0) st_uv: vec2<f32>) -> FragmentOutput {
    st_uv_1 = st_uv;
    main();
    let _e24 = st_color;
    return FragmentOutput(_e24);
}
