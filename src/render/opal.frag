// The built-in wallpaper (D11): a base colour under four soft elliptical blobs, as in the Main/Dark mockups
// (CSS `radial-gradient(ellipse rx ry at x y, c, transparent 70%)`, first blob on top).

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

uniform float alpha;
uniform vec2 size;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec3 base;
// rgb + alpha at each blob's centre.
uniform vec4 blob1;
uniform vec4 blob2;
uniform vec4 blob3;
uniform vec4 blob4;

vec4 blob(vec4 c, vec2 centre, vec2 radii) {
    float t = length((v_coords - centre) / radii) / 0.7;
    float a = c.a * (1.0 - clamp(t, 0.0, 1.0));
    return vec4(c.rgb * a, a);
}

vec4 over(vec4 top, vec4 bottom) {
    return top + bottom * (1.0 - top.a);
}

void main() {
    vec4 c = vec4(base, 1.0);
    c = over(blob(blob4, vec2(0.40, 0.60), vec2(0.40, 0.35)), c);
    c = over(blob(blob3, vec2(0.62, 0.88), vec2(0.60, 0.50)), c);
    c = over(blob(blob2, vec2(0.82, 0.22), vec2(0.55, 0.50)), c);
    c = over(blob(blob1, vec2(0.18, 0.28), vec2(0.60, 0.55)), c);
    // Half a step of noise hides 8-bit banding in the long, soft gradients.
    float noise = fract(sin(dot(v_coords * size, vec2(12.9898, 78.233))) * 43758.5453) - 0.5;
    c.rgb += noise / 255.0;

    vec4 color = c * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
