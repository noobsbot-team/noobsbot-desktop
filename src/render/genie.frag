// The genie minimise (E5.5): a window snapshot warped so each row is one source row, squeezed towards the dock.
// Same maths as `anim::Genie`, in coordinates normalised to the element's box (0..1 both ways).

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// The window's area and the target, (x, y, w, h); squeeze and the top and bottom rows' heights.
uniform vec4 area;
uniform vec4 target;
uniform vec3 span;
// Maps v_coords to physical px in the box: p = v_coords * to_px.xy + to_px.zw; the box's size in physical px.
uniform vec4 to_px;
uniform vec2 box;

float step2(float a, float b, float x) {
    return b - a < 1e-6 ? step(b, x) : smoothstep(a, b, x);
}

void main() {
    vec2 p = (v_coords * to_px.xy + to_px.zw) / box;
    float m = span.x * step2(area.y, target.y, p.y);
    float l = mix(area.x, target.x, m);
    float r = mix(area.x + area.z, target.x + target.z, m);
    vec2 src = vec2(clamp((p.x - l) / max(r - l, 1e-6), 0.0, 1.0), clamp((p.y - span.y) / max(span.z - span.y, 1e-6), 0.0, 1.0));
    // The whole source fills the box undeformed: sample where that puts (u, row).
    vec4 color = texture2D(tex, (src * box - to_px.zw) / to_px.xy);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    // A one-pixel soft edge on the sides.
    color *= clamp(min(p.x - l, r - p.x) * box.x + 0.5, 0.0, 1.0);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
