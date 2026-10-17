// Window content clipped to rounded corners (D5: radius 10). Smithay's texture shader plus a coverage mask.
// ponytail: circular arcs, not Apple's continuous squircle; swap rrect() for a superellipse SDF when it shows.

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

// Window geometry size in physical px, corner radius in physical px.
uniform vec2 geo_size;
uniform float radius;
// Maps v_coords to a position in the window geometry: p = v_coords * to_geo.xy + to_geo.zw.
uniform vec4 to_geo;

float rrect(vec2 p, vec2 size, float r) {
    vec2 q = abs(p - size * 0.5) - size * 0.5 + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    vec2 p = v_coords * to_geo.xy + to_geo.zw;
    color *= clamp(0.5 - rrect(p, geo_size, radius), 0.0, 1.0);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
