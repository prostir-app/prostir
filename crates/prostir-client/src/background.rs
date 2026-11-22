//! The "panorama" background: an equirectangular image shown by the compositor
//! as a sphere around the user. Generated here (a calm night sky) unless the
//! user supplies their own picture.

/// Width × height of the generated panorama (2:1 equirectangular).
pub const WIDTH: u32 = 4096;
pub const HEIGHT: u32 = 2048;
/// The longest side of a panorama's picture the headset takes as it is.
pub const MAX_SIDE: u32 = 4096;

/// The box the user's picture of `width` × `height` is made smaller to fit (keeping
/// its proportions), when either side is longer than [`MAX_SIDE`].
pub fn fit_within(width: u32, height: u32) -> Option<(u32, u32)> {
    (width > MAX_SIDE || height > MAX_SIDE).then_some((WIDTH, HEIGHT))
}

/// Small deterministic PRNG (xorshift) so the sky looks the same every launch.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Smooth value noise in [0, 1], wrapping horizontally every `period` cells.
fn noise(x: f32, y: f32, period: u32, seed: u64) -> f32 {
    let hash = |ix: i64, iy: i64| {
        let ix = ix.rem_euclid(period as i64) as u64;
        let mut h = ix.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (iy as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ seed;
        h ^= h >> 29;
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 32;
        (h >> 40) as f32 / (1u64 << 24) as f32
    };
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (ix, iy) = (x0 as i64, y0 as i64);
    let a = hash(ix, iy) + (hash(ix + 1, iy) - hash(ix, iy)) * s(fx);
    let b = hash(ix, iy + 1) + (hash(ix + 1, iy + 1) - hash(ix, iy + 1)) * s(fx);
    a + (b - a) * s(fy)
}

/// RGBA8 (opaque, sRGB), top row = straight up.
/// Built-in panoramas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sky {
    Night,
    Sunset,
    Space,
    Aurora,
}

pub fn generate(sky: Sky) -> Vec<u8> {
    match sky {
        Sky::Night => night_sky(),
        Sky::Sunset => sunset(),
        Sky::Space => space(),
        Sky::Aurora => aurora(),
    }
}

/// Fractal noise: a few octaves of `noise`, in [0, 1].
fn fbm(x: f32, y: f32, period: u32, seed: u64, octaves: u32) -> f32 {
    let (mut sum, mut amp, mut norm, mut f) = (0.0, 1.0, 0.0, 1.0);
    for o in 0..octaves {
        sum += noise(x * f, y * f, period * f as u32, seed + o as u64) * amp;
        norm += amp;
        amp *= 0.5;
        f *= 2.0;
    }
    sum / norm
}

/// Unit direction for a panorama point: `u` around (0.5 = straight ahead), `elev` in [-1, 1].
fn direction(u: f32, elev: f32) -> [f32; 3] {
    let phi = (u - 0.5) * std::f32::consts::TAU;
    let theta = elev * std::f32::consts::FRAC_PI_2;
    [
        theta.cos() * phi.sin(),
        theta.sin(),
        -theta.cos() * phi.cos(),
    ]
}

/// Fills the panorama from a linear-colour function of `(u, elev)`, using all cores.
fn render(shade: impl Fn(f32, f32) -> [f32; 3] + Sync) -> Vec<u8> {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let mut out = vec![0u8; w * h * 4];
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let rows_per = h.div_ceil(threads);
    std::thread::scope(|scope| {
        for (chunk_index, chunk) in out.chunks_mut(rows_per * w * 4).enumerate() {
            let shade = &shade;
            scope.spawn(move || {
                for (r, row) in chunk.chunks_mut(w * 4).enumerate() {
                    let y = chunk_index * rows_per + r;
                    let elev = 1.0 - 2.0 * (y as f32 + 0.5) / h as f32;
                    for x in 0..w {
                        let c = shade(x as f32 / w as f32, elev);
                        for k in 0..3 {
                            row[x * 4 + k] = (to_srgb(c[k]) * 255.0).round() as u8;
                        }
                        row[x * 4 + 3] = 255;
                    }
                }
            });
        }
    });
    out
}

/// Scatters stars above `min_elev` (use -1 for the whole sphere).
fn stars(out: &mut [u8], count: u32, min_elev: f32, seed: u64, strength: f32) {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let mut rng = Rng(seed);
    for _ in 0..count {
        let x = rng.unit() * w as f32;
        // Uniform on the sphere: sin(elevation angle) uniform.
        let elev = (2.0 * rng.unit() - 1.0).asin() / std::f32::consts::FRAC_PI_2;
        if elev < min_elev {
            continue;
        }
        let y = (1.0 - elev) * 0.5 * h as f32;
        // Equirect pixels get narrower towards the poles: stretch stars to stay round.
        let stretch = 1.0 / (elev * std::f32::consts::FRAC_PI_2).cos().max(0.05);
        let brightness = (rng.unit().powf(6.0) * 0.9 + 0.08) * strength;
        let color = mix([0.75, 0.82, 1.0], [1.0, 0.88, 0.75], rng.unit());
        let radius = 0.6 + brightness * 1.4;
        plot_star(out, w, h, x, y, radius * stretch, radius, brightness, color);
    }
}

/// Calm night sky with a warm glow on the horizon.
pub fn night_sky() -> Vec<u8> {
    let zenith = [0.014, 0.019, 0.042];
    let sky = [0.030, 0.037, 0.080];
    let horizon = [0.085, 0.075, 0.140];
    let glow = [0.300, 0.170, 0.230];
    let ground = [0.018, 0.020, 0.032];
    let mut out = render(|u, elev| {
        let mut c = if elev >= 0.0 {
            let base = mix(horizon, sky, (elev / 0.25).powf(0.7));
            mix(base, zenith, ((elev - 0.25) / 0.75).max(0.0))
        } else {
            mix(horizon, ground, (-elev / 0.08).powf(0.6))
        };
        if elev > 0.0 {
            let n = noise(u * 24.0, elev * 10.0, 24, 7) * 0.6
                + noise(u * 64.0, elev * 26.0, 64, 11) * 0.4;
            let band = (1.0 - ((elev - 0.18) / 0.16).abs()).max(0.0);
            let k = band * n * n * 0.10;
            c = [c[0] + k * 0.25, c[1] + k * 0.55, c[2] + k * 0.75];
        }
        let g = (-(elev.abs() / 0.045)).exp() * (0.55 + 0.45 * noise(u * 8.0, 0.0, 8, 3));
        mix(c, glow, g * 0.6)
    });
    stars(&mut out, 18000, 0.04, 0x5EED_CAFE_F00D, 1.0);
    out
}

/// Sun just under the horizon ahead, lit clouds, dark hills.
pub fn sunset() -> Vec<u8> {
    let zenith = [0.020, 0.030, 0.090];
    let mid = [0.150, 0.090, 0.200];
    let horizon = [0.700, 0.260, 0.120];
    let sun_u = 0.47;
    let mut out = render(|u, elev| {
        // Hills: a ragged silhouette a few degrees high.
        let hill = 0.010 + 0.075 * fbm(u * 7.0, 0.5, 7, 21, 5).powf(1.6);
        if elev < hill {
            let rim = (1.0 - (hill - elev) / 0.01).max(0.0) * 0.05;
            let depth = (-elev).max(0.0);
            let k = 1.0 - (depth / 0.4).min(1.0) * 0.7;
            return [(0.022 + rim) * k, (0.012 + rim * 0.4) * k, 0.032 * k];
        }
        let t = (elev / 0.35).clamp(0.0, 1.0);
        let mut c = mix(horizon, mid, t.powf(0.6));
        c = mix(c, zenith, ((elev - 0.3) / 0.7).max(0.0).powf(0.8));
        // Sun glow.
        let du = (u - sun_u).abs().min(1.0 - (u - sun_u).abs()) * 4.0;
        let d2 = du * du + (elev - 0.01) * (elev - 0.01) * 16.0;
        let glow = (-d2 / 0.03).exp() + (-d2 / 0.4).exp() * 0.25;
        c = [c[0] + glow * 1.2, c[1] + glow * 0.55, c[2] + glow * 0.18];
        // Clouds in a band, lit pink near the sun and violet away from it.
        let band = (1.0 - ((elev - 0.14) / 0.10).abs()).max(0.0);
        let n = fbm(u * 16.0, elev * 18.0, 16, 5, 5);
        let cloud = ((n - 0.48) * 4.0).clamp(0.0, 1.0) * band;
        let lit = mix(
            [0.25, 0.10, 0.20],
            [1.00, 0.45, 0.35],
            (-du * du / 0.3).exp(),
        );
        mix(c, lit, cloud * 0.85)
    });
    stars(&mut out, 4000, 0.45, 0xBEEF, 0.5);
    out
}

/// Deep space: coloured nebulae, a galactic band, many stars.
pub fn space() -> Vec<u8> {
    // Plane of the galactic band, tilted across the sky.
    let n = {
        let v: [f32; 3] = [0.35, 0.80, 0.48];
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        [v[0] / l, v[1] / l, v[2] / l]
    };
    let mut out = render(|u, elev| {
        let d = direction(u, elev);
        let dist = d[0] * n[0] + d[1] * n[1] + d[2] * n[2];
        let mut c = [0.003, 0.004, 0.010];
        let neb = fbm(u * 12.0, elev * 6.0, 12, 77, 5);
        let hue = fbm(u * 5.0, elev * 3.0, 5, 91, 3);
        let k = ((neb - 0.45) * 2.5).clamp(0.0, 1.0).powf(2.0) * 0.35;
        let tint = mix([0.30, 0.04, 0.40], [0.02, 0.28, 0.34], hue);
        c = [c[0] + tint[0] * k, c[1] + tint[1] * k, c[2] + tint[2] * k];
        let band = (-(dist * dist) / 0.004).exp() + (-(dist * dist) / 0.03).exp() * 0.25;
        let dust = fbm(u * 60.0, elev * 30.0, 60, 13, 5);
        let b = band * (0.25 + 0.75 * dust).powf(2.0) * 0.16;
        // Dark lanes through the band.
        let lane = ((dust - 0.58) * 5.0).clamp(0.0, 1.0) * band.min(1.0);
        [
            (c[0] + b * 1.0) * (1.0 - lane * 0.8),
            (c[1] + b * 0.86) * (1.0 - lane * 0.8),
            (c[2] + b * 0.70) * (1.0 - lane * 0.8),
        ]
    });
    stars(&mut out, 40000, -1.0, 0xC0FFEE, 1.0);
    out
}

/// Polar night: green-to-violet curtains over a snowy plain.
pub fn aurora() -> Vec<u8> {
    let mut out = render(|u, elev| {
        if elev < 0.0 {
            // Snow: brightest near the horizon, where it catches the aurora.
            let k = (-(-elev) / 0.12).exp();
            return [0.008 + k * 0.012, 0.012 + k * 0.035, 0.024 + k * 0.03];
        }
        let mut c = mix(
            [0.012, 0.024, 0.045],
            [0.003, 0.005, 0.016],
            (elev / 0.6).min(1.0),
        );
        // Curtains fold and sway: bend the horizontal coordinate with height.
        let fold =
            0.015 * (elev * 18.0 + u * 40.0).sin() + 0.04 * fbm(u * 4.0, elev * 2.0, 4, 61, 3);
        let uu = u + fold;
        let base = 0.05 + 0.16 * fbm(uu * 3.0, 0.0, 3, 31, 3);
        if elev > base {
            let h = (elev - base) / 0.4;
            let fade = (1.0 - h).max(0.0).powf(2.0) * (h * 10.0).min(1.0);
            let streak = fbm(uu * 70.0, elev * 1.5, 70, 47, 3);
            let curtain = fbm(uu * 6.0, 0.3, 6, 53, 4);
            let k = fade * streak.powf(1.6) * ((curtain - 0.38) * 3.0).clamp(0.0, 1.0) * 0.8;
            let col = mix([0.08, 0.80, 0.42], [0.40, 0.12, 0.62], (h * 1.3).min(1.0));
            c = [c[0] + col[0] * k, c[1] + col[1] * k, c[2] + col[2] * k];
        }
        c
    });
    stars(&mut out, 12000, 0.05, 0xA0A0, 0.8);
    out
}

#[allow(clippy::too_many_arguments)]
fn plot_star(
    out: &mut [u8],
    w: usize,
    h: usize,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    brightness: f32,
    color: [f32; 3],
) {
    let (x0, x1) = (
        (cx - rx - 1.0).floor() as i64,
        (cx + rx + 1.0).ceil() as i64,
    );
    let (y0, y1) = (
        (cy - ry - 1.0).floor() as i64,
        (cy + ry + 1.0).ceil() as i64,
    );
    for y in y0.max(0)..=y1.min(h as i64 - 1) {
        for x in x0..=x1 {
            let dx = (x as f32 + 0.5 - cx) / rx;
            let dy = (y as f32 + 0.5 - cy) / ry;
            let a = (-(dx * dx + dy * dy) * 2.5).exp() * brightness;
            if a < 0.004 {
                continue;
            }
            let xi = x.rem_euclid(w as i64) as usize;
            let i = (y as usize * w + xi) * 4;
            for k in 0..3 {
                let lin = from_srgb(out[i + k] as f32 / 255.0) + color[k] * a;
                out[i + k] = (to_srgb(lin.min(1.0)) * 255.0).round() as u8;
            }
        }
    }
}

fn to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn from_srgb(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    /// A picture too wide or too tall is made smaller; one within the limit is not.
    #[test]
    fn a_picture_over_the_limit_either_way_is_made_smaller() {
        assert_eq!(super::fit_within(4096, 2048), None);
        assert_eq!(super::fit_within(4096, 4096), None);
        assert_eq!(super::fit_within(8192, 4096), Some((4096, 2048)));
        assert_eq!(super::fit_within(3000, 5000), Some((4096, 2048)), "tall");
    }
    #[test]
    fn sky_is_opaque_and_dark_below() {
        let px = super::night_sky();
        assert_eq!(px.len(), (super::WIDTH * super::HEIGHT * 4) as usize);
        assert!(px.chunks(4).all(|p| p[3] == 255));
        let bottom = ((super::HEIGHT - 1) * super::WIDTH * 4) as usize;
        assert!(
            px[bottom] < 50 && px[bottom + 2] < 60,
            "{:?}",
            &px[bottom..bottom + 4]
        );
    }
}
