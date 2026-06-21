//! CPU rasteriser for language text badges into SNI ARGB32 pixmaps.
//!
//! Port of `src/tray/icon_badge.c`. The tray base icon's floor (ADR-0032) is a
//! 1–3 glyph label in the language's own script (中 / あ / الد / EN). The
//! StatusNotifierItem `IconPixmap` channel carries raw ARGB32 bitmaps, so the
//! host must rasterise the glyphs itself — the flux/GPU glyph atlas is an R8
//! coverage texture for the Vulkan composer and cannot feed D-Bus.
//!
//! This unit is independent of the panel/flux stack. The C version used
//! FreeType + HarfBuzz + Fontconfig; this Rust port uses the pure-Rust
//! `rustybuzz` (shaping — Arabic badges such as الد require contextual
//! joining), `fontdb` (find a face covering the badge's script), and
//! `ab_glyph` (outline rasterisation). Output bytes are big-endian ARGB32 as
//! SNI / KStatusNotifierItem require.

use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, Glyph, GlyphId, PxScale};
use fontdb::Database;
use rustybuzz::{Face as RbFace, UnicodeBuffer};

/// One rasterised badge bitmap at a single size. `argb` is `width*height`
/// pixels, 4 bytes each, big-endian ARGB32 (SNI byte order: `[A, R, G, B]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadgePixmap {
    pub width: i32,
    pub height: i32,
    pub argb: Vec<u8>,
}

/// Process-lifetime system font database (lazily loaded once).
fn font_db() -> &'static Database {
    static DB: OnceLock<Database> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = Database::new();
        db.load_system_fonts();
        db
    })
}

/// Find owned font bytes + face index for the first face whose cmap covers
/// `cp`. Mirrors the C `badge_match_font`.
fn match_font(cp: char) -> Option<(Vec<u8>, u32)> {
    let db = font_db();
    for face in db.faces() {
        let covered = db
            .with_face_data(face.id, |data, index| {
                rustybuzz::ttf_parser::Face::parse(data, index)
                    .ok()
                    .and_then(|f| f.glyph_index(cp))
                    .is_some()
            })
            .unwrap_or(false);
        if !covered {
            continue;
        }
        if let Some(owned) = db.with_face_data(face.id, |data, index| (data.to_vec(), index)) {
            return Some(owned);
        }
    }
    None
}

/// Source-over blend of a premultiplied-by-`a` colour into the ARGB32 buffer.
fn blend(buf: &mut [u8], idx: usize, r: u8, g: u8, b: u8, a: f32) {
    let a = a.clamp(0.0, 1.0);
    if a <= 0.0 {
        return;
    }
    let inv = 1.0 - a;
    // Stored order is [A, R, G, B].
    let da = buf[idx] as f32 / 255.0;
    let out_a = a + da * inv;
    let blend_ch = |dst: u8, src: u8| -> u8 {
        ((src as f32 * a) + (dst as f32 * da * inv) / out_a.max(1e-6))
            .round()
            .clamp(0.0, 255.0) as u8
    };
    buf[idx] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    buf[idx + 1] = blend_ch(buf[idx + 1], r);
    buf[idx + 2] = blend_ch(buf[idx + 2], g);
    buf[idx + 3] = blend_ch(buf[idx + 3], b);
}

/// Rasterise the shaped run into one `size`×`size` ARGB32 pixmap.
fn render_one(
    ab_font: &FontRef,
    rb_face: &RbFace,
    text: &str,
    size: u32,
    fg_rgb: u32,
) -> Option<BadgePixmap> {
    let size_i = size as i32;
    let dim = size as usize;
    if dim == 0 {
        return None;
    }

    // Shape the badge text.
    let mut buf = UnicodeBuffer::new();
    buf.push_str(text);
    buf.guess_segment_properties();
    let shaped = rustybuzz::shape(rb_face, &[], buf);
    let infos = shaped.glyph_infos();
    let positions = shaped.glyph_positions();
    if infos.is_empty() {
        return None;
    }

    let upem = rb_face.units_per_em() as f32;
    if upem <= 0.0 {
        return None;
    }
    // Leave a margin; px is the em size in pixels.
    let px = size as f32 * 0.82;
    let sf = px / upem;
    let scale = PxScale::from(px);

    // Total advance (font units → px) to centre the run horizontally.
    let total_adv: f32 = positions.iter().map(|p| p.x_advance as f32).sum::<f32>() * sf;
    let mut pen_x = (size as f32 - total_adv) / 2.0;
    let baseline = size as f32 * 0.78;

    let r = ((fg_rgb >> 16) & 0xFF) as u8;
    let g = ((fg_rgb >> 8) & 0xFF) as u8;
    let b = (fg_rgb & 0xFF) as u8;

    let mut argb = vec![0u8; dim * dim * 4];
    let mut any_coverage = false;

    // Outline offsets (thin dark halo) then the foreground pass on top.
    let passes: [(i32, i32, u8, u8, u8); 9] = [
        (-1, -1, 0, 0, 0),
        (0, -1, 0, 0, 0),
        (1, -1, 0, 0, 0),
        (-1, 0, 0, 0, 0),
        (1, 0, 0, 0, 0),
        (-1, 1, 0, 0, 0),
        (0, 1, 0, 0, 0),
        (1, 1, 0, 0, 0),
        (0, 0, r, g, b), // foreground, drawn last
    ];

    for (info, pos) in infos.iter().zip(positions.iter()) {
        let gx = pen_x + pos.x_offset as f32 * sf;
        let gy = baseline - pos.y_offset as f32 * sf;
        pen_x += pos.x_advance as f32 * sf;

        let glyph = Glyph {
            id: GlyphId(info.glyph_id as u16),
            scale,
            position: ab_glyph::point(gx, gy),
        };
        let Some(outline) = ab_font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outline.px_bounds();
        for &(ox, oy, pr, pg, pb) in &passes {
            outline.draw(|x, y, coverage| {
                if coverage <= 0.0 {
                    return;
                }
                let px_x = bounds.min.x as i32 + x as i32 + ox;
                let px_y = bounds.min.y as i32 + y as i32 + oy;
                if px_x < 0 || px_y < 0 || px_x >= size_i || px_y >= size_i {
                    return;
                }
                let idx = ((px_y as usize) * dim + px_x as usize) * 4;
                blend(&mut argb, idx, pr, pg, pb, coverage);
                any_coverage = true;
            });
        }
    }

    if !any_coverage {
        return None;
    }
    Some(BadgePixmap {
        width: size_i,
        height: size_i,
        argb,
    })
}

/// Rasterise `text` into one pixmap per requested size.
///
/// `text` is a short UTF-8 badge (1–3 glyphs), single script. `sizes` lists
/// square pixel sizes (e.g. `[22, 44]` for HiDPI). `fg_rgb` is the glyph
/// colour as `0xRRGGBB`. Returns one [`BadgePixmap`] per size, or an empty
/// `Vec` on any failure (invalid input, no covering font, nothing rendered) —
/// the caller then falls back to an icon name. All-or-nothing: a partial
/// failure yields an empty `Vec`.
pub fn render(text: &str, sizes: &[u32], fg_rgb: u32) -> Vec<BadgePixmap> {
    if text.is_empty() || sizes.is_empty() {
        return Vec::new();
    }
    let Some(first_cp) = text.chars().next() else {
        return Vec::new();
    };
    let Some((bytes, face_index)) = match_font(first_cp) else {
        return Vec::new();
    };
    let Ok(ab_font) = FontRef::try_from_slice_and_index(&bytes, face_index) else {
        return Vec::new();
    };
    let Some(rb_face) = RbFace::from_slice(&bytes, face_index) else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(sizes.len());
    for &size in sizes {
        match render_one(&ab_font, &rb_face, text, size, fg_rgb) {
            Some(pixmap) => out.push(pixmap),
            None => return Vec::new(), // all-or-nothing
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_coverage(p: &BadgePixmap) -> bool {
        // Any non-zero alpha byte (alpha is byte 0 of each ARGB32 pixel).
        p.argb.chunks_exact(4).any(|px| px[0] != 0)
    }

    #[test]
    fn rejects_bad_input() {
        assert!(render("", &[22], 0xFFFFFF).is_empty());
        assert!(render("X", &[], 0xFFFFFF).is_empty());
    }

    #[test]
    fn renders_latin_badge() {
        let sizes = [22u32, 44];
        let out = render("EN", &sizes, 0xFFFFFF);
        // Requires a Latin-covering system font; skip gracefully if none.
        if out.is_empty() {
            eprintln!("no covering font for Latin in this env — skipping");
            return;
        }
        assert_eq!(out.len(), 2);
        for (i, p) in out.iter().enumerate() {
            assert_eq!(p.width, sizes[i] as i32);
            assert_eq!(p.height, sizes[i] as i32);
            assert_eq!(p.argb.len(), (sizes[i] * sizes[i] * 4) as usize);
            assert!(has_coverage(p), "size {} produced no coverage", sizes[i]);
        }
    }

    #[test]
    fn renders_cjk_badge_when_font_present() {
        // CJK fonts (Noto Sans KR/HK/TC) are present in this env.
        let out = render("中", &[44], 0xFFFFFF);
        if out.is_empty() {
            eprintln!("no covering CJK font in this env — skipping");
            return;
        }
        assert_eq!(out.len(), 1);
        assert!(has_coverage(&out[0]));
    }
}
