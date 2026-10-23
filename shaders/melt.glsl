// melt: the shadow of a float melts the text under it. The letters themselves flow: a smooth
// field moves them down and sideways like syrup, they stretch into drips, and where they were
// only the cell's background is left. Their colour stays their own.

// smooth 2D value noise and a few octaves of it (a field with no grain, so it flows)
float melt_hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
float melt_noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    vec2 u = f * f * (3.0 - 2.0 * f);
    return mix(mix(melt_hash(i), melt_hash(i + vec2(1.0, 0.0)), u.x), mix(melt_hash(i + vec2(0.0, 1.0)), melt_hash(i + vec2(1.0, 1.0)), u.x), u.y);
}
float melt_fbm(vec2 p) { return 0.55 * melt_noise(p) + 0.3 * melt_noise(p * 2.03 + 5.2) + 0.15 * melt_noise(p * 4.01 + 1.7); }

void mainShadow(out vec4 fragColor, in vec2 px) {
    // how much it melts here: nothing at the shadow's faint edge, most next to the float
    float s = smoothstep(0.02, 0.45, zShadow);
    vec2 p = px / zCell;
    float t = iTime;
    // gravity: everything sags down, more in some places than others, slowly changing
    float sag = zCell * 0.9 * s * (0.55 + 0.45 * melt_fbm(vec2(p.x * 0.6, t * 0.25)));
    // the liquid's own swirl: a warped field (noise of noise), so it moves like a fluid
    vec2 q = vec2(melt_fbm(p * 0.5 + vec2(0.0, t * 0.35)), melt_fbm(p * 0.5 + vec2(4.7, -t * 0.3)));
    vec2 warp = (vec2(melt_fbm(p * 0.7 + 2.0 * q + vec2(t * 0.2, 0.0)), melt_fbm(p * 0.7 + 2.0 * q + vec2(1.3, t * 0.25))) - 0.5) * zCell * 0.35 * s;
    // where this pixel's ink comes from: above it (it ran down) and off to the side (it swirled)
    vec2 src = px - vec2(0.0, sag) + warp;
    // a drip: the letter stretched down, its ink thinning along the run
    float len = zCell * 1.4 * s * melt_fbm(vec2(p.x * 1.3, t * 0.4));
    vec3 ink = vec3(0.0);
    float amount = 0.0;
    for (int k = 0; k < 10; k++) {
        float f = float(k) / 9.0;
        vec2 at = src - vec2(0.0, len * f * f);
        // a float's own letters stay where they are
        if (zInFloat(at)) { break; }
        float m = zText(at) * (1.0 - 0.75 * f);
        if (m > amount) { amount = m; ink = scene(at).rgb; }
    }
    // the cell's own background here (the letter has gone from it), in the shadow; the ink that
    // flowed here over it, in its own colour
    vec3 shaded = zPlain(px).rgb * (1.0 - zShadow);
    fragColor = vec4(mix(shaded, ink, amount), 1.0);
}
