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
///
/// Layout strategy:
/// 1. Shape the text and lay it out at the font's unit scale
///    (`1 em == upem px`), measuring the combined glyph-bbox in font units.
/// 2. Compute a scale-to-fit factor so that bbox just fills the canvas
///    minus a 1px halo on every side (room for the dark outline).
/// 3. Re-layout at that scale, shift-centre the resulting bbox, and draw
///    an 8-direction 1px dark outline followed by the foreground.
///
/// The scale-to-fit fixes the long-standing issue that fixed-factor
/// layouts (`px = size * 0.82`) rendered Latin badges ("EN") at ~60% of
/// the canvas while CJK badges ("中") already filled it — different
/// scripts have very different glyph-box/em ratios, and the only way to
/// get a uniform visual size is to measure the real bbox.
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

    let r = ((fg_rgb >> 16) & 0xFF) as u8;
    let g = ((fg_rgb >> 8) & 0xFF) as u8;
    let b = (fg_rgb & 0xFF) as u8;

    // ── Pass 1: measure at unit scale (1 em = upem px) so we get the
    //    natural glyph bbox in font units, independent of any pixel scale.
    let unit_scale = PxScale::from(upem);
    let mut measure_bbox: Option<ab_glyph::Rect> = None;
    let mut pen_x_units = 0.0_f32;
    for (info, pos) in infos.iter().zip(positions.iter()) {
        let gx = pen_x_units + pos.x_offset as f32;
        let gy = -pos.y_offset as f32;
        pen_x_units += pos.x_advance as f32;
        let glyph = Glyph {
            id: GlyphId(info.glyph_id as u16),
            scale: unit_scale,
            position: ab_glyph::point(gx, gy),
        };
        let Some(outline) = ab_font.outline_glyph(glyph) else {
            continue;
        };
        let bb = outline.px_bounds();
        measure_bbox = Some(match measure_bbox {
            Some(mut acc) => {
                acc.min.x = acc.min.x.min(bb.min.x);
                acc.min.y = acc.min.y.min(bb.min.y);
                acc.max.x = acc.max.x.max(bb.max.x);
                acc.max.y = acc.max.y.max(bb.max.y);
                acc
            }
            None => bb,
        });
    }

    let mbbox = measure_bbox?;
    let mbbox_w = mbbox.max.x - mbbox.min.x;
    let mbbox_h = mbbox.max.y - mbbox.min.y;
    if mbbox_w < 1.0 || mbbox_h < 1.0 {
        return None;
    }

    // ── Scale-to-fit: pick the largest scale whose bbox fits in the canvas
    //    minus a halo on each side. The halo gives the dark outline room to
    //    draw its neighbours without clipping at the canvas edge and without
    //    visually merging with adjacent UI; 1px is the sweet spot between
    //    "badge fills the pixmap" and "badge has breathing room".
    let halo = 1.0_f32;
    let target = (size as f32 - 2.0 * halo).max(8.0);
    let fit_factor = (target / mbbox_w).min(target / mbbox_h);
    let px = fit_factor * upem;
    let sf = fit_factor;
    let scale = PxScale::from(px);

    // ── Pass 2: build outlines at the chosen scale and recompute the
    //    combined bbox so we can shift-centre it precisely.
    let mut outlines: Vec<ab_glyph::OutlinedGlyph> = Vec::new();
    let mut bbox: Option<ab_glyph::Rect> = None;
    let mut pen_x = 0.0_f32;
    for (info, pos) in infos.iter().zip(positions.iter()) {
        let gx = pen_x + pos.x_offset as f32 * sf;
        let gy = -pos.y_offset as f32 * sf;
        pen_x += pos.x_advance as f32 * sf;
        let glyph = Glyph {
            id: GlyphId(info.glyph_id as u16),
            scale,
            position: ab_glyph::point(gx, gy),
        };
        let Some(outline) = ab_font.outline_glyph(glyph) else {
            continue;
        };
        let bb = outline.px_bounds();
        bbox = Some(match bbox {
            Some(mut acc) => {
                acc.min.x = acc.min.x.min(bb.min.x);
                acc.min.y = acc.min.y.min(bb.min.y);
                acc.max.x = acc.max.x.max(bb.max.x);
                acc.max.y = acc.max.y.max(bb.max.y);
                acc
            }
            None => bb,
        });
        outlines.push(outline);
    }

    let bbox = bbox?;
    let bbox_w = bbox.max.x - bbox.min.x;
    let bbox_h = bbox.max.y - bbox.min.y;
    if bbox_w < 1.0 || bbox_h < 1.0 || outlines.is_empty() {
        return None;
    }

    let shift_x = (size as f32 - bbox_w) / 2.0 - bbox.min.x;
    let shift_y = (size as f32 - bbox_h) / 2.0 - bbox.min.y;

    let mut argb = vec![0u8; dim * dim * 4];
    let mut any_coverage = false;

    // Outline strategy:
    // * 24px and up — 8-direction 1px halo. Glyphs are large enough that
    //   the diagonal neighbours don't close up counter-forms (the inside
    //   of 中 / あ / EN stays open).
    // * Below 24px — 4-direction 1px halo plus a second 4-direction pass
    //   at ±2px. That thickens the dark fringe without adding diagonal
    //   coverage that would fill in the middle of dense CJK glyphs.
    // Foreground is drawn last so the white sits on top of the dark halo
    // along the glyph edge.
    let outline_passes_large: [(i32, i32, u8, u8, u8); 9] = [
        (-1, -1, 0, 0, 0),
        (0, -1, 0, 0, 0),
        (1, -1, 0, 0, 0),
        (-1, 0, 0, 0, 0),
        (1, 0, 0, 0, 0),
        (-1, 1, 0, 0, 0),
        (0, 1, 0, 0, 0),
        (1, 1, 0, 0, 0),
        (0, 0, r, g, b),
    ];
    let outline_passes_small: [(i32, i32, u8, u8, u8); 9] = [
        (-1, 0, 0, 0, 0),
        (1, 0, 0, 0, 0),
        (0, -1, 0, 0, 0),
        (0, 1, 0, 0, 0),
        (-2, 0, 0, 0, 0),
        (2, 0, 0, 0, 0),
        (0, -2, 0, 0, 0),
        (0, 2, 0, 0, 0),
        (0, 0, r, g, b),
    ];
    let passes: &[(i32, i32, u8, u8, u8)] = if size >= 24 {
        &outline_passes_large
    } else {
        &outline_passes_small
    };

    for outline in &outlines {
        let bounds = outline.px_bounds();
        let min_x = bounds.min.x + shift_x;
        let min_y = bounds.min.y + shift_y;
        for &(ox, oy, pr, pg, pb) in passes {
            outline.draw(|x, y, coverage| {
                if coverage <= 0.0 {
                    return;
                }
                let px_x = min_x as i32 + x as i32 + ox;
                let px_y = min_y as i32 + y as i32 + oy;
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

    /// Debug aid: dump a badge as a PAM file under /tmp/badge_dbg/ for visual
    /// inspection of pixel sharpness / centering. Ignored by default; invoke
    /// with `cargo test -p typio-host --lib icon_badge::tests::dump_for_inspection -- --nocapture --ignored`.
    #[test]
    #[ignore]
    fn dump_for_inspection() {
        let _ = std::fs::create_dir("/tmp/badge_dbg");
        for text in ["中", "EN", "あ", "Рус"] {
            for &size in [16u32, 22, 24, 32, 64].iter() {
                let out = render(text, &[size], 0xFFFFFF);
                if out.is_empty() {
                    continue;
                }
                let p = &out[0];
                let path = format!("/tmp/badge_dbg/{text}_{size}.pam");
                if let Ok(mut f) = std::fs::File::create(&path) {
                    use std::io::Write;
                    let _ = write!(
                        f,
                        "P7\nWIDTH {}\nHEIGHT {}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n",
                        p.width, p.height
                    );
                    let _ = f.write_all(&p.argb);
                    let _ = writeln!(
                        std::io::stderr(),
                        "dumped {path} ({}x{})",
                        p.width, p.height
                    );
                }
            }
        }
    }
}
