// Soft drop shadow of a rounded rect (D6) plus its 0.5 px outer hairline, drawn only outside the rect.
// The element covers the rect grown by `margin` on every side.
// ponytail: per-pixel SDF + logistic falloff (exact on straight edges, close at corners), every frame a shadow is
// damaged; render once per (radius, blur, opacity) into a 9-slice texture (plan E1.4) when 4K GPU time needs it.

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

uniform float alpha;
// Element size in logical px (set by Smithay); `scale` turns it into physical px.
uniform vec2 size;
uniform float scale;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// All lengths in physical px.
uniform float margin;
uniform float offset_y;
// Gaussian sigma (CSS blur radius / 2).
uniform float sigma;
uniform float radius;
uniform float opacity;
// Black alpha of the 0.5 px hairline.
uniform float hairline;

float rrect(vec2 p, vec2 size, float r) {
    vec2 q = abs(p - size * 0.5) - size * 0.5 + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

// How much of the pixel centred at distance d lies in the band [lo, hi].
float band(float d, float lo, float hi) {
    return clamp(min(d + 0.5, hi) - max(d - 0.5, lo), 0.0, 1.0);
}

void main() {
    vec2 box = size * scale - 2.0 * margin;
    vec2 p = v_coords * size * scale - margin;
    float d_rect = rrect(p, box, radius);
    float d_shadow = rrect(p - vec2(0.0, offset_y), box, radius);

    // 1 - Φ(d / σ), with the logistic approximation of the normal CDF.
    float shadow = opacity / (1.0 + exp(1.702 * d_shadow / max(sigma, 0.001)));
    shadow *= clamp(0.5 + d_rect, 0.0, 1.0);
    float hair = hairline * band(d_rect, 0.0, 0.5 * scale);
    float a = hair + shadow * (1.0 - hair);

    vec4 color = vec4(0.0, 0.0, 0.0, a) * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
