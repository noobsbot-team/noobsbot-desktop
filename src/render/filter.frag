// Zoom and colour filters (E8.6): the composed frame, through `mode`: 0 none, 1 invert, 2 invert keeping hue,
// 3 greyscale. Smithay's texture shader with one more step; colours are premultiplied.

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

uniform float mode;

void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color.a = 1.0;
#endif
    if (mode == 1.0) {
        color.rgb = color.a - color.rgb;
    } else if (mode == 2.0) {
        // Lightness flipped, hue and saturation kept: every channel moves by the same amount.
        float hi = max(color.r, max(color.g, color.b));
        float lo = min(color.r, min(color.g, color.b));
        color.rgb += color.a - hi - lo;
    } else if (mode == 3.0) {
        color.rgb = vec3(dot(color.rgb, vec3(0.2126, 0.7152, 0.0722)));
    }
    color *= alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
