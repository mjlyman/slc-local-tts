//! The app's icon, drawn in code so every size and every badge comes from
//! one source: a teal rounded square with five white bars, like a short
//! waveform, and an optional badge in the bottom-right corner. The exe's
//! `.ico` file is generated from [`render`] too.

use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::WindowsAndMessaging::SM_CXSMICON;

use super::model::Badge;

/// Teal `#0E7C86`, the square.
pub const TEAL: [u8; 3] = [0x0E, 0x7C, 0x86];
/// Cyan `#56C6E3`, the download badge.
pub const CYAN: [u8; 3] = [0x56, 0xC6, 0xE3];
/// Amber `#F2AD45`, the attention badge.
pub const AMBER: [u8; 3] = [0xF2, 0xAD, 0x45];
const WHITE: [u8; 3] = [0xFF, 0xFF, 0xFF];

/// Each bar's height, as a share of the square's inner height, left to
/// right.
const BAR_HEIGHTS: [f32; 5] = [0.36, 0.64, 0.92, 0.56, 0.28];

/// The icon at `size` × `size` pixels with `badge`, as RGBA bytes (red,
/// green, blue, alpha; alpha not premultiplied), row by row from the top.
pub fn render(size: u32, badge: Badge) -> Vec<u8> {
    let side = size as f32;
    let radius = side / 5.0;
    let bars = bars(size);
    let disc = Disc::badge(size);
    let arrow = Arrow::inside(&disc, size);

    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut pixel = Pixel::default();
            let square = coverage(x, y, |px, py| in_rounded_square(px, py, side, radius));
            pixel.paint(TEAL, square);
            let bar = coverage(x, y, |px, py| bars.iter().any(|b| b.contains(px, py)));
            pixel.paint(WHITE, bar);
            match badge {
                Badge::None => {}
                Badge::Downloading => {
                    pixel.paint(CYAN, coverage(x, y, |px, py| disc.contains(px, py)));
                    pixel.paint(WHITE, coverage(x, y, |px, py| arrow.contains(px, py)));
                }
                Badge::Attention => {
                    pixel.paint(AMBER, coverage(x, y, |px, py| disc.contains(px, py)));
                }
            }
            out.extend_from_slice(&pixel.to_rgba());
        }
    }
    out
}

/// The tray's icon, at the size Windows uses for small icons at the
/// screen's scale (16 pixels at 100%, 24 at 150%, 32 at 200%).
pub fn tray_icon(badge: Badge) -> tray_icon::Icon {
    // SAFETY: plain Win32 calls with no pointers.
    let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) };
    // 0 means the call failed; 16 is the size at 100% scale.
    let size = u32::try_from(size).ok().filter(|&s| s > 0).unwrap_or(16);
    tray_icon::Icon::from_rgba(render(size, badge), size, size)
        .expect("render always returns size × size RGBA pixels")
}

/// How much of pixel (`x`, `y`) a shape covers, from 0 to 1: the share of
/// a 4 × 4 grid of points inside the pixel for which `inside` is true. This
/// softens the edges (antialiasing).
fn coverage(x: u32, y: u32, inside: impl Fn(f32, f32) -> bool) -> f32 {
    const GRID: u32 = 4;
    let mut hits = 0;
    for j in 0..GRID {
        for i in 0..GRID {
            let px = x as f32 + (i as f32 + 0.5) / GRID as f32;
            let py = y as f32 + (j as f32 + 0.5) / GRID as f32;
            if inside(px, py) {
                hits += 1;
            }
        }
    }
    hits as f32 / (GRID * GRID) as f32
}

/// True if (`x`, `y`) is inside a `side` × `side` square with corners
/// rounded to `radius`.
fn in_rounded_square(x: f32, y: f32, side: f32, radius: f32) -> bool {
    // The nearest point of the square shrunk by `radius` on every side;
    // the rounded square is everything within `radius` of it.
    let nx = x.clamp(radius, side - radius);
    let ny = y.clamp(radius, side - radius);
    (x - nx).powi(2) + (y - ny).powi(2) <= radius * radius
}

/// One pixel while it's painted, layer by layer. The colour is kept
/// premultiplied by alpha (each channel already scaled by how opaque the
/// pixel is), which makes painting one layer over another a plain blend.
#[derive(Clone, Copy, Default)]
struct Pixel {
    red: f32,
    green: f32,
    blue: f32,
    alpha: f32,
}

impl Pixel {
    /// Paints `color` over the pixel, covering `amount` (0 to 1) of it.
    fn paint(&mut self, color: [u8; 3], amount: f32) {
        let keep = 1.0 - amount;
        let [r, g, b] = color.map(|c| f32::from(c) / 255.0);
        self.red = r * amount + self.red * keep;
        self.green = g * amount + self.green * keep;
        self.blue = b * amount + self.blue * keep;
        self.alpha = amount + self.alpha * keep;
    }

    /// The pixel as RGBA bytes, with alpha no longer premultiplied.
    fn to_rgba(self) -> [u8; 4] {
        if self.alpha <= 0.0 {
            return [0; 4];
        }
        let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
        [
            byte(self.red / self.alpha),
            byte(self.green / self.alpha),
            byte(self.blue / self.alpha),
            byte(self.alpha),
        ]
    }
}

/// One bar, in whole pixels: its left column, width, and top and bottom
/// edges (which may fall inside a pixel).
#[derive(Clone, Copy, Debug)]
struct Bar {
    left: u32,
    width: u32,
    top: f32,
    bottom: f32,
}

impl Bar {
    fn contains(&self, x: f32, y: f32) -> bool {
        let left = self.left as f32;
        x >= left && x < left + self.width as f32 && y >= self.top && y < self.bottom
    }
}

/// Where the five bars go at `size`. They sit in the square's inner area
/// (the square less a margin as wide as the corner radius), evenly spaced
/// across it and centred vertically. Their left edges fall on whole pixels,
/// so even 1-pixel bars stay sharp.
fn bars(size: u32) -> [Bar; 5] {
    let side = size as f32;
    let inner = side - 2.0 * (side / 5.0);
    let width = (size / 10).max(1);
    let gap = ((inner - 5.0 * width as f32) / 4.0).round().max(1.0) as u32;
    let group = 5 * width + 4 * gap;
    let start = size.saturating_sub(group) / 2;
    // `from_fn` builds the array by calling the closure for each index.
    std::array::from_fn(|i| {
        let height = BAR_HEIGHTS[i] * inner;
        Bar {
            left: start + i as u32 * (width + gap),
            width,
            top: (side - height) / 2.0,
            bottom: (side + height) / 2.0,
        }
    })
}

/// The badge's circle: its diameter is half the icon (rounded up), and it
/// sits in the bottom-right corner.
struct Disc {
    /// The same for x and y, since the corner is on the diagonal.
    centre: f32,
    radius: f32,
}

impl Disc {
    fn badge(size: u32) -> Disc {
        let diameter = size.div_ceil(2) as f32;
        Disc {
            centre: size as f32 - diameter / 2.0,
            radius: diameter / 2.0,
        }
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        (x - self.centre).powi(2) + (y - self.centre).powi(2) <= self.radius * self.radius
    }
}

/// A point, as (x, y).
type Point = (f32, f32);

/// The download badge's white down-arrow: a shaft and a two-stroke head,
/// with strokes 1 pixel wide below 24 pixels and 2 from 24.
struct Arrow {
    /// Line segments, as (start, end).
    strokes: [(Point, Point); 3],
    half_width: f32,
}

impl Arrow {
    fn inside(disc: &Disc, size: u32) -> Arrow {
        let thin = size < 24;
        let diameter = disc.radius * 2.0;
        // A 1-pixel stroke is centred on a pixel's middle, and a 2-pixel
        // one on the line between two pixels, so both stay sharp.
        let x = if thin {
            disc.centre.floor() + 0.5
        } else {
            disc.centre.round()
        };
        let top = disc.centre - 0.30 * diameter;
        let tip = disc.centre + 0.26 * diameter;
        let wing = 0.22 * diameter;
        Arrow {
            strokes: [
                ((x, top), (x, tip)),
                ((x - wing, tip - wing), (x, tip)),
                ((x + wing, tip - wing), (x, tip)),
            ],
            half_width: if thin { 0.5 } else { 1.0 },
        }
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        self.strokes
            .iter()
            .any(|&(a, b)| distance_to_segment((x, y), a, b) <= self.half_width)
    }
}

/// The distance from `p` to the nearest point of the segment from `a` to
/// `b`.
fn distance_to_segment(p: Point, a: Point, b: Point) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length_squared = dx * dx + dy * dy;
    // How far along the segment the nearest point is: 0 at `a`, 1 at `b`.
    let t = if length_squared == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length_squared).clamp(0.0, 1.0)
    };
    let (nx, ny) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - nx).powi(2) + (p.1 - ny).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(image: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        [image[i], image[i + 1], image[i + 2], image[i + 3]]
    }

    /// True if any fully opaque pixel in the bottom-right quadrant has
    /// exactly `color`.
    fn quadrant_has(image: &[u8], size: u32, color: [u8; 3]) -> bool {
        (size / 2..size).any(|y| {
            (size / 2..size).any(|x| {
                let [r, g, b, a] = pixel(image, size, x, y);
                a == 255 && [r, g, b] == color
            })
        })
    }

    fn anywhere_has(image: &[u8], color: [u8; 3]) -> bool {
        image
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[3] == 255 && p[..3] == color)
    }

    #[test]
    fn icon_render_sizes_and_badges() {
        for size in [16, 24, 32] {
            for badge in [Badge::None, Badge::Downloading, Badge::Attention] {
                let image = render(size, badge);
                assert_eq!(image.len(), (size * size * 4) as usize, "{size} {badge:?}");
                assert_eq!(pixel(&image, size, 0, 0)[3], 0, "{size} {badge:?}: corner");

                let middle = bars(size)[2];
                let column = middle.left + middle.width / 2;
                assert_eq!(
                    pixel(&image, size, column, size / 2),
                    [255, 255, 255, 255],
                    "{size} {badge:?}: middle bar"
                );
                assert!(
                    anywhere_has(&image, TEAL),
                    "{size} {badge:?}: the square is teal"
                );

                let cyan = quadrant_has(&image, size, CYAN);
                let amber = quadrant_has(&image, size, AMBER);
                match badge {
                    Badge::None => {
                        assert!(!anywhere_has(&image, CYAN), "{size}: no cyan");
                        assert!(!anywhere_has(&image, AMBER), "{size}: no amber");
                    }
                    Badge::Downloading => {
                        assert!(cyan, "{size}: cyan badge");
                        assert!(!anywhere_has(&image, AMBER), "{size}: no amber");
                    }
                    Badge::Attention => {
                        assert!(amber, "{size}: amber badge");
                        assert!(!anywhere_has(&image, CYAN), "{size}: no cyan");
                    }
                }
            }
        }
    }

    #[test]
    fn bars_are_evenly_spaced_and_inside() {
        for size in [16, 20, 24, 32, 40, 48, 64, 256] {
            let bars = bars(size);
            let width = (size / 10).max(1);
            let gaps: Vec<u32> = bars
                .windows(2)
                .map(|pair| pair[1].left - (pair[0].left + pair[0].width))
                .collect();
            assert!(
                gaps.iter().all(|&g| g == gaps[0] && g >= 1),
                "{size}: {gaps:?}"
            );
            for bar in bars {
                assert_eq!(bar.width, width, "{size}");
                assert!(bar.left + bar.width <= size, "{size}");
                // Centred vertically.
                let middle = (bar.top + bar.bottom) / 2.0;
                assert!((middle - size as f32 / 2.0).abs() < 0.01, "{size}");
            }
            // The tallest bar is the middle one.
            let heights: Vec<f32> = bars.iter().map(|b| b.bottom - b.top).collect();
            assert!(heights[2] > heights[1] && heights[1] > heights[3]);
        }
    }

    #[test]
    fn edges_are_antialiased() {
        // Some pixel along the rounded corner is partly transparent.
        let image = render(32, Badge::None);
        assert!(
            image
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[3] > 0 && p[3] < 255)
        );
    }
}
