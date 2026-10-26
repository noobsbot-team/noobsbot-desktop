// A glass material (D3, no blur): tint + top sheen + 1 px inner highlight (brighter top edge) + 0.5 px outer hairline.
// The element covers the rounded rect grown by `inset` on every side, room for the hairline.
// ponytail: circular corners, not a continuous squircle; swap rrect() for a superellipse SDF when it shows.

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

uniform float alpha;
// Element size in logical px (set by Smithay); `px` is physical px per logical px.
uniform vec2 size;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// Tint rgb + opacity (not premultiplied).
uniform vec4 fill;
uniform float px;
// Lengths in physical px.
uniform float inset;
uniform float radius;
// White alpha at the top of the sheen gradient, of the highlight ring and of its top edge; black alpha of the hairline.
uniform float sheen;
uniform float highlight;
uniform float highlight_top;
uniform float hairline;
// Above 0, only the outer `hollow` physical px of the shape are drawn: a ring (focus rings, D7).
uniform float hollow;
// Increase contrast (E8.6): a 1 px border just inside the edge, premultiplied rgba.
uniform vec4 border;

float rrect(vec2 p, vec2 size, float r) {
    vec2 q = abs(p - size * 0.5) - size * 0.5 + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

float band(float d, float lo, float hi) {
    return clamp(min(d + 0.5, hi) - max(d - 0.5, lo), 0.0, 1.0);
}

vec4 over(vec4 top, vec4 bottom) {
    return top + bottom * (1.0 - top.a);
}

void main() {
    vec2 box = size * px - 2.0 * inset;
    vec2 p = v_coords * size * px - inset;
    float d = rrect(p, box, radius);
    float inside = clamp(0.5 - d, 0.0, 1.0);
    if (hollow > 0.0) inside *= clamp(d + hollow + 0.5, 0.0, 1.0);

    vec4 c = vec4(fill.rgb * fill.a, fill.a);
    c = over(vec4(1.0) * sheen * clamp(1.0 - p.y / box.y, 0.0, 1.0), c);
    c = over(vec4(1.0) * highlight * band(d, -px, 0.0), c);
    // CSS `inset 0 1px 0`: inside the rect but outside the rect moved down by 1 px.
    float top = clamp(0.5 + rrect(p - vec2(0.0, px), box, radius), 0.0, 1.0);
    c = over(vec4(1.0) * highlight_top * top, c);
    c = over(border * band(d, -px, 0.0), c);
    c *= inside;
    c = over(c, vec4(0.0, 0.0, 0.0, hairline * band(d, 0.0, 0.5 * px)));

    vec4 color = c * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
