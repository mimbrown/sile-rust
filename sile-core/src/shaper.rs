use crate::font::{Direction, FontFace, FontSpec};
use crate::length::Length;
use crate::measurement::Measurement;

// ---------------------------------------------------------------------------
// GlyphItem
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GlyphItem {
    pub gid: u16,
    pub cluster: u32,
    pub text: String,
    pub width: f64,
    pub height: f64,
    pub depth: f64,
    pub x_offset: f64,
    pub y_offset: f64,
    pub x_advance: f64,
    pub y_advance: f64,
}

// ---------------------------------------------------------------------------
// CharMetrics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct CharMetrics {
    pub width: f64,
    pub height: f64,
    pub depth: f64,
}

// ---------------------------------------------------------------------------
// SpaceSettings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct SpaceSettings {
    pub variable_spaces: bool,
    pub enlargement_factor: f64,
    pub stretch_factor: f64,
    pub shrink_factor: f64,
}

impl Default for SpaceSettings {
    fn default() -> Self {
        Self {
            variable_spaces: true,
            enlargement_factor: 1.0,
            stretch_factor: 0.5,
            shrink_factor: 1.0 / 3.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Shaper trait
// ---------------------------------------------------------------------------

pub trait Shaper {
    fn shape(&self, text: &str, face: &FontFace, spec: &FontSpec) -> Vec<GlyphItem>;

    fn measure_char(&self, c: char, face: &FontFace, spec: &FontSpec) -> (CharMetrics, bool) {
        let items = self.shape(&c.to_string(), face, spec);
        let mut width = 0.0_f64;
        let mut height = 0.0_f64;
        let mut depth = 0.0_f64;
        let mut found = false;
        for item in &items {
            width += item.width;
            height = height.max(item.height);
            depth = depth.max(item.depth);
            if item.gid != 0 {
                found = true;
            }
        }
        (CharMetrics { width, height, depth }, found)
    }

    fn measure_space(
        &self,
        face: &FontFace,
        spec: &FontSpec,
        settings: &SpaceSettings,
    ) -> Length {
        let items = self.shape(" ", face, spec);
        let raw_width = items.first().map(|g| g.width).unwrap_or(0.0);

        if !settings.variable_spaces {
            return Length::pt(raw_width);
        }

        let base = raw_width.abs();
        Length::new(
            Measurement::pt(base * settings.enlargement_factor),
            Measurement::pt(base * settings.stretch_factor),
            Measurement::pt(base * settings.shrink_factor),
        )
    }
}

// ---------------------------------------------------------------------------
// RustyBuzzShaper
// ---------------------------------------------------------------------------

pub struct RustyBuzzShaper;

impl RustyBuzzShaper {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RustyBuzzShaper {
    fn default() -> Self {
        Self::new()
    }
}

impl Shaper for RustyBuzzShaper {
    fn shape(&self, text: &str, face: &FontFace, spec: &FontSpec) -> Vec<GlyphItem> {
        let (data, index) = face.raw_data();
        let rb_face = match rustybuzz::Face::from_slice(data, index) {
            Some(f) => f,
            None => return vec![],
        };

        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);

        buffer.set_direction(match spec.direction {
            Direction::LTR => rustybuzz::Direction::LeftToRight,
            Direction::RTL => rustybuzz::Direction::RightToLeft,
            Direction::TTB => rustybuzz::Direction::TopToBottom,
        });

        if !spec.script.is_empty() {
            if let Ok(script) = spec.script.parse::<rustybuzz::Script>() {
                buffer.set_script(script);
            }
        }

        if !spec.language.is_empty() {
            if let Ok(lang) = spec.language.parse::<rustybuzz::Language>() {
                buffer.set_language(lang);
            }
        }

        let features = parse_features(&spec.features);
        let glyph_buffer = rustybuzz::shape(&rb_face, &features, buffer);

        let infos = glyph_buffer.glyph_infos();
        let positions = glyph_buffer.glyph_positions();
        let scale = spec.size / face.units_per_em() as f64;

        let texts = extract_glyph_texts(text, infos);

        let mut items = Vec::with_capacity(infos.len());
        for i in 0..infos.len() {
            let gid = infos[i].glyph_id as u16;

            let (height, depth) = face
                .glyph_bounding_box(gid)
                .map(|bb| (bb.y_max as f64 * scale, -(bb.y_min as f64) * scale))
                .unwrap_or((0.0, 0.0));

            items.push(GlyphItem {
                gid,
                cluster: infos[i].cluster,
                text: texts[i].clone(),
                width: positions[i].x_advance as f64 * scale,
                height,
                depth,
                x_offset: positions[i].x_offset as f64 * scale,
                y_offset: positions[i].y_offset as f64 * scale,
                x_advance: positions[i].x_advance as f64 * scale,
                y_advance: positions[i].y_advance as f64 * scale,
            });
        }

        items
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_features(features_str: &str) -> Vec<rustybuzz::Feature> {
    if features_str.is_empty() {
        return vec![];
    }
    features_str
        .split(',')
        .filter_map(|s| s.trim().parse::<rustybuzz::Feature>().ok())
        .collect()
}

fn extract_glyph_texts(text: &str, infos: &[rustybuzz::GlyphInfo]) -> Vec<String> {
    if infos.is_empty() {
        return vec![];
    }

    let mut clusters: Vec<u32> = infos.iter().map(|g| g.cluster).collect();
    clusters.sort_unstable();
    clusters.dedup();

    let text_len = text.len() as u32;
    let mut cluster_end = std::collections::HashMap::with_capacity(clusters.len());
    for i in 0..clusters.len() {
        let end = if i + 1 < clusters.len() {
            clusters[i + 1]
        } else {
            text_len
        };
        cluster_end.insert(clusters[i], end);
    }

    infos
        .iter()
        .map(|info| {
            let start = info.cluster as usize;
            let end = *cluster_end.get(&info.cluster).unwrap_or(&text_len) as usize;
            let end = end.min(text.len());
            let start = start.min(end);
            text[start..end].to_string()
        })
        .collect()
}

/// Apply tracking (letter-spacing) to shaped glyphs. Modifies `width` but
/// preserves `x_advance` / `y_advance` as the original shaper values.
pub fn apply_tracking(items: &mut [GlyphItem], factor: f64) {
    for item in items.iter_mut() {
        item.width *= factor;
    }
}

/// Shape text with font fallback. Tries the primary face first; any glyphs
/// with gid=0 are re-shaped with each fallback face in order.
pub fn shape_with_fallbacks(
    shaper: &dyn Shaper,
    text: &str,
    primary_face: &FontFace,
    primary_spec: &FontSpec,
    fallbacks: &[(FontFace, FontSpec)],
) -> Vec<GlyphItem> {
    let mut items = shaper.shape(text, primary_face, primary_spec);

    if fallbacks.is_empty() || items.iter().all(|g| g.gid != 0) {
        return items;
    }

    for (fb_face, fb_spec) in fallbacks {
        let mut all_resolved = true;
        for item in items.iter_mut() {
            if item.gid != 0 {
                continue;
            }
            let fb_items = shaper.shape(&item.text, fb_face, fb_spec);
            if let Some(fb) = fb_items.first() {
                if fb.gid != 0 {
                    *item = fb.clone();
                } else {
                    all_resolved = false;
                }
            }
        }
        if all_resolved {
            break;
        }
    }

    items
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn load_system_font_and_spec() -> Option<(FontFace, FontSpec)> {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let info = db.faces().next()?;
        let family = info.families.first()?.0.clone();
        let id = info.id;
        let mut data_out: Option<(Vec<u8>, u32)> = None;
        db.with_face_data(id, |data, index| {
            data_out = Some((data.to_vec(), index));
        });
        let (data, index) = data_out?;
        let face = FontFace::from_bytes(data, index).ok()?;
        let spec = FontSpec {
            family: Some(family),
            size: 12.0,
            ..Default::default()
        };
        Some((face, spec))
    }

    // -- Feature parsing -----------------------------------------------------

    #[test]
    fn parse_empty_features() {
        assert!(parse_features("").is_empty());
    }

    #[test]
    fn parse_single_feature() {
        let features = parse_features("kern");
        assert_eq!(features.len(), 1);
    }

    #[test]
    fn parse_multiple_features() {
        let features = parse_features("+kern,-liga,+dlig");
        assert_eq!(features.len(), 3);
    }

    // -- Shaping -------------------------------------------------------------

    #[test]
    fn shape_hello() {
        let (face, spec) = match load_system_font_and_spec() {
            Some(v) => v,
            None => return,
        };
        let shaper = RustyBuzzShaper::new();
        let items = shaper.shape("Hello", &face, &spec);
        assert_eq!(items.len(), 5);
        for item in &items {
            assert!(item.width > 0.0, "each glyph should have positive width");
            assert_ne!(item.gid, 0, "each glyph should be found");
        }
    }

    #[test]
    fn shape_single_char() {
        let (face, spec) = match load_system_font_and_spec() {
            Some(v) => v,
            None => return,
        };
        let shaper = RustyBuzzShaper::new();
        let items = shaper.shape("A", &face, &spec);
        assert_eq!(items.len(), 1);
        assert_ne!(items[0].gid, 0);
        assert!(items[0].width > 0.0);
        assert!(items[0].height > 0.0);
        assert_eq!(items[0].text, "A");
    }

    #[test]
    fn shape_preserves_cluster_text() {
        let (face, spec) = match load_system_font_and_spec() {
            Some(v) => v,
            None => return,
        };
        let shaper = RustyBuzzShaper::new();
        let items = shaper.shape("AB", &face, &spec);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "A");
        assert_eq!(items[1].text, "B");
    }

    #[test]
    fn shape_space_has_width() {
        let (face, spec) = match load_system_font_and_spec() {
            Some(v) => v,
            None => return,
        };
        let shaper = RustyBuzzShaper::new();
        let items = shaper.shape(" ", &face, &spec);
        assert_eq!(items.len(), 1);
        assert!(items[0].width > 0.0, "space should have positive width");
    }

    // -- Measure char --------------------------------------------------------

    #[test]
    fn measure_char_a() {
        let (face, spec) = match load_system_font_and_spec() {
            Some(v) => v,
            None => return,
        };
        let shaper = RustyBuzzShaper::new();
        let (metrics, found) = shaper.measure_char('A', &face, &spec);
        assert!(found);
        assert!(metrics.width > 0.0);
        assert!(metrics.height > 0.0);
    }

    #[test]
    fn measure_char_ab_wider_than_a() {
        let (face, spec) = match load_system_font_and_spec() {
            Some(v) => v,
            None => return,
        };
        let shaper = RustyBuzzShaper::new();
        let (a_metrics, _) = shaper.measure_char('A', &face, &spec);
        let items = shaper.shape("AB", &face, &spec);
        let ab_width: f64 = items.iter().map(|g| g.width).sum();
        assert!(ab_width > a_metrics.width);
    }

    // -- Measure space -------------------------------------------------------

    #[test]
    fn measure_space_variable() {
        let (face, spec) = match load_system_font_and_spec() {
            Some(v) => v,
            None => return,
        };
        let shaper = RustyBuzzShaper::new();
        let settings = SpaceSettings::default();
        let space = shaper.measure_space(&face, &spec, &settings);
        let w = space.to_pt_abs();
        assert!(w > 0.0, "space width should be positive");
    }

    #[test]
    fn measure_space_fixed() {
        let (face, spec) = match load_system_font_and_spec() {
            Some(v) => v,
            None => return,
        };
        let shaper = RustyBuzzShaper::new();
        let settings = SpaceSettings {
            variable_spaces: false,
            ..Default::default()
        };
        let space = shaper.measure_space(&face, &spec, &settings);
        // Fixed spaces have zero stretch/shrink
        let stretch = space.stretch.to_pt_abs();
        let shrink = space.shrink.to_pt_abs();
        assert_eq!(stretch, 0.0);
        assert_eq!(shrink, 0.0);
    }

    // -- Tracking ------------------------------------------------------------

    #[test]
    fn apply_tracking_scales_width() {
        let (face, spec) = match load_system_font_and_spec() {
            Some(v) => v,
            None => return,
        };
        let shaper = RustyBuzzShaper::new();
        let mut items = shaper.shape("A", &face, &spec);
        let original = items[0].width;
        let original_advance = items[0].x_advance;
        apply_tracking(&mut items, 1.5);
        let expected = original * 1.5;
        assert!((items[0].width - expected).abs() < 1e-10);
        // x_advance should be unchanged
        assert_eq!(items[0].x_advance, original_advance);
    }

    // -- Direction -----------------------------------------------------------

    #[test]
    fn shape_with_rtl() {
        let (face, mut spec) = match load_system_font_and_spec() {
            Some(v) => v,
            None => return,
        };
        spec.direction = Direction::RTL;
        let shaper = RustyBuzzShaper::new();
        let items = shaper.shape("AB", &face, &spec);
        // RTL shaping should still produce glyphs
        assert!(!items.is_empty());
        for item in &items {
            assert!(item.width > 0.0);
        }
    }

    // -- Fallback ------------------------------------------------------------

    #[test]
    fn shape_with_fallbacks_no_missing() {
        let (face, spec) = match load_system_font_and_spec() {
            Some(v) => v,
            None => return,
        };
        let shaper = RustyBuzzShaper::new();
        let items = shape_with_fallbacks(&shaper, "Hello", &face, &spec, &[]);
        assert_eq!(items.len(), 5);
    }

    // -- Empty input ---------------------------------------------------------

    #[test]
    fn shape_empty_string() {
        let (face, spec) = match load_system_font_and_spec() {
            Some(v) => v,
            None => return,
        };
        let shaper = RustyBuzzShaper::new();
        let items = shaper.shape("", &face, &spec);
        assert!(items.is_empty());
    }
}
