use std::path::Path;
use std::sync::Arc;

use crate::color::Color;
use crate::font::{FontDatabase, FontError, FontFace, FontSpec};
use crate::frame::{FrameConstraint, PageLayout, PaperSize};
use crate::hyphenation::HyphenationDictionary;
use crate::length::Length;
use crate::linebreak::{self, BreakResult, LinebreakSettings};
use crate::measurement::Measurement;
use crate::node::{self, GlyphData, NNode, Node, VBox};
use crate::pagebuilder::{PageBreakSettings, PageBuilder};
use crate::pdf::{Bookmark, PdfConfig, PdfError, PdfOutputter};
use crate::shaper::{GlyphItem, RustyBuzzShaper, Shaper, SpaceSettings};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum BuilderError {
    Font(FontError),
    Pdf(PdfError),
    NoFont(String),
    Layout(String),
}

impl std::fmt::Display for BuilderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Font(e) => write!(f, "{e}"),
            Self::Pdf(e) => write!(f, "{e}"),
            Self::NoFont(name) => write!(f, "no font registered with name \"{name}\""),
            Self::Layout(msg) => write!(f, "layout error: {msg}"),
        }
    }
}

impl std::error::Error for BuilderError {}

impl From<FontError> for BuilderError {
    fn from(e: FontError) -> Self {
        Self::Font(e)
    }
}

impl From<PdfError> for BuilderError {
    fn from(e: PdfError) -> Self {
        Self::Pdf(e)
    }
}

// ---------------------------------------------------------------------------
// FontEntry (internal)
// ---------------------------------------------------------------------------

struct RegisteredFont {
    spec: FontSpec,
    face: Arc<FontFace>,
}

// ---------------------------------------------------------------------------
// TextRun (internal)
// ---------------------------------------------------------------------------

struct TextRun {
    text: String,
    font_name: String,
    color: Option<Color>,
}

// ---------------------------------------------------------------------------
// DocumentBuilder
// ---------------------------------------------------------------------------

pub struct DocumentBuilder {
    // Page geometry
    paper: PaperSize,
    margins: [f64; 4], // top, right, bottom, left
    header_height: f64,
    footer_height: f64,
    frame_gap: f64,

    // Font system
    font_db: FontDatabase,
    fonts: std::collections::HashMap<String, RegisteredFont>,
    shaper: RustyBuzzShaper,
    current_font: Option<String>,

    // Hyphenation
    hyphenation: HyphenationDictionary,
    language: String,

    // Style state
    current_color: Option<Color>,

    // Paragraph state
    paragraph_runs: Vec<TextRun>,
    paragraph_indent: f64,
    paragraph_skip: f64,
    leading: f64,
    space_settings: SpaceSettings,
    first_paragraph: bool,

    // Settings
    linebreak_settings: LinebreakSettings,
    page_break_settings: PageBreakSettings,

    // Accumulated vertical content
    vertical_queue: Vec<Node>,

    // PDF config
    pdf_config: PdfConfig,
    bookmarks: Vec<Bookmark>,
    page_count: usize,
}

impl DocumentBuilder {
    pub fn new(paper: PaperSize) -> Self {
        Self {
            paper,
            margins: [72.0; 4],
            header_height: 0.0,
            footer_height: 0.0,
            frame_gap: 0.0,
            font_db: FontDatabase::new(),
            fonts: std::collections::HashMap::new(),
            shaper: RustyBuzzShaper::new(),
            current_font: None,
            hyphenation: HyphenationDictionary::new(),
            language: "en".to_string(),
            current_color: None,
            paragraph_runs: Vec::new(),
            paragraph_indent: 20.0,
            paragraph_skip: 0.0,
            leading: 2.0,
            space_settings: SpaceSettings::default(),
            first_paragraph: true,
            linebreak_settings: LinebreakSettings::default(),
            page_break_settings: PageBreakSettings::default(),
            vertical_queue: Vec::new(),
            pdf_config: PdfConfig::default(),
            bookmarks: Vec::new(),
            page_count: 0,
        }
    }

    // -- Page geometry -------------------------------------------------------

    pub fn set_page_size(&mut self, paper: PaperSize) -> &mut Self {
        self.paper = paper;
        self
    }

    pub fn set_margins(&mut self, top: f64, right: f64, bottom: f64, left: f64) -> &mut Self {
        self.margins = [top, right, bottom, left];
        self
    }

    pub fn set_header_height(&mut self, height: f64, gap: f64) -> &mut Self {
        self.header_height = height;
        self.frame_gap = gap;
        self
    }

    pub fn set_footer_height(&mut self, height: f64, gap: f64) -> &mut Self {
        self.footer_height = height;
        self.frame_gap = gap;
        self
    }

    // -- Font management -----------------------------------------------------

    pub fn load_system_fonts(&mut self) -> &mut Self {
        self.font_db.load_system_fonts();
        self
    }

    pub fn load_font_file(
        &mut self,
        name: impl Into<String>,
        path: impl AsRef<Path>,
        spec: FontSpec,
    ) -> Result<&mut Self, BuilderError> {
        let data = std::fs::read(path.as_ref())
            .map_err(|e| FontError::Io(e.to_string()))?;
        self.load_font_data(name, data, spec)
    }

    pub fn load_font_data(
        &mut self,
        name: impl Into<String>,
        data: Vec<u8>,
        spec: FontSpec,
    ) -> Result<&mut Self, BuilderError> {
        let face = Arc::new(FontFace::from_bytes(data, 0)?);
        let name = name.into();
        self.fonts.insert(name, RegisteredFont { spec, face });
        Ok(self)
    }

    pub fn load_font_by_family(
        &mut self,
        name: impl Into<String>,
        spec: FontSpec,
    ) -> Result<&mut Self, BuilderError> {
        let face = self.font_db.resolve(&spec)?;
        let name = name.into();
        self.fonts.insert(name, RegisteredFont { spec, face });
        Ok(self)
    }

    pub fn set_font(&mut self, name: impl Into<String>) -> &mut Self {
        self.current_font = Some(name.into());
        self
    }

    pub fn set_font_size(&mut self, size: f64) -> &mut Self {
        if let Some(ref name) = self.current_font.clone()
            && let Some(entry) = self.fonts.get_mut(name) {
                entry.spec.size = size;
            }
        self
    }

    // -- Language and hyphenation --------------------------------------------

    pub fn set_language(&mut self, lang: impl Into<String>) -> &mut Self {
        self.language = lang.into();
        self.hyphenation.load_language(&self.language);
        self
    }

    // -- Style ---------------------------------------------------------------

    pub fn set_color(&mut self, color: Color) -> &mut Self {
        self.current_color = Some(color);
        self
    }

    pub fn clear_color(&mut self) -> &mut Self {
        self.current_color = None;
        self
    }

    // -- Paragraph settings --------------------------------------------------

    pub fn set_paragraph_indent(&mut self, indent: f64) -> &mut Self {
        self.paragraph_indent = indent;
        self
    }

    pub fn set_paragraph_skip(&mut self, skip: f64) -> &mut Self {
        self.paragraph_skip = skip;
        self
    }

    pub fn set_leading(&mut self, leading: f64) -> &mut Self {
        self.leading = leading;
        self
    }

    pub fn set_space_settings(&mut self, settings: SpaceSettings) -> &mut Self {
        self.space_settings = settings;
        self
    }

    pub fn linebreak_settings_mut(&mut self) -> &mut LinebreakSettings {
        &mut self.linebreak_settings
    }

    pub fn page_break_settings_mut(&mut self) -> &mut PageBreakSettings {
        &mut self.page_break_settings
    }

    // -- Text ----------------------------------------------------------------

    pub fn add_text(&mut self, text: impl Into<String>) -> &mut Self {
        let font_name = self.current_font.clone().unwrap_or_default();
        let color = self.current_color;
        self.paragraph_runs.push(TextRun {
            text: text.into(),
            font_name,
            color,
        });
        self
    }

    pub fn new_paragraph(&mut self) -> Result<&mut Self, BuilderError> {
        if self.paragraph_runs.is_empty() {
            return Ok(self);
        }

        let runs = std::mem::take(&mut self.paragraph_runs);
        let nodes = self.typeset_paragraph(&runs)?;

        // Inter-paragraph skip
        if !self.first_paragraph && self.paragraph_skip > 0.0 {
            self.vertical_queue.push(Node::vglue(Length::new(
                Measurement::pt(self.paragraph_skip),
                Measurement::pt(self.paragraph_skip * 0.5),
                Measurement::pt(0.0),
            )));
            self.vertical_queue.push(Node::penalty(0));
        }

        self.vertical_queue.extend(nodes);
        self.first_paragraph = false;
        Ok(self)
    }

    // -- Vertical material ---------------------------------------------------

    pub fn add_vskip(&mut self, amount: f64) -> &mut Self {
        self.vertical_queue.push(Node::vglue(Length::pt(amount)));
        self
    }

    pub fn add_page_break(&mut self) -> &mut Self {
        self.vertical_queue.push(Node::penalty(-10_000));
        self
    }

    pub fn add_rule(&mut self, width: f64, height: f64) -> &mut Self {
        // A rule is an HBox with dimensions
        let vbox = VBox {
            width: Length::pt(width),
            height: Length::pt(height),
            depth: Length::zero(),
            nodes: vec![Node::hbox(width, height, 0.0)],
            misfit: false,
            explicit: false,
        };
        self.vertical_queue.push(Node::VBox(vbox));
        self
    }

    // -- Bookmarks and links ------------------------------------------------

    pub fn add_bookmark(&mut self, title: impl Into<String>, level: u32) -> &mut Self {
        self.bookmarks.push(Bookmark {
            title: title.into(),
            page_index: self.page_count,
            level,
            y_position: self.margins[0],
        });
        self
    }

    // -- PDF config ----------------------------------------------------------

    pub fn set_title(&mut self, title: impl Into<String>) -> &mut Self {
        self.pdf_config.title = Some(title.into());
        self
    }

    pub fn set_author(&mut self, author: impl Into<String>) -> &mut Self {
        self.pdf_config.author = Some(author.into());
        self
    }

    pub fn set_subject(&mut self, subject: impl Into<String>) -> &mut Self {
        self.pdf_config.subject = Some(subject.into());
        self
    }

    pub fn set_compress(&mut self, compress: bool) -> &mut Self {
        self.pdf_config.compress = compress;
        self
    }

    // -- Render --------------------------------------------------------------

    pub fn render(mut self) -> Result<Vec<u8>, BuilderError> {
        // Flush any pending paragraph
        if !self.paragraph_runs.is_empty() {
            // We need to move self to call new_paragraph, which takes &mut self
            let runs = std::mem::take(&mut self.paragraph_runs);
            let nodes = self.typeset_paragraph(&runs)?;
            if !self.first_paragraph && self.paragraph_skip > 0.0 {
                self.vertical_queue.push(Node::vglue(Length::new(
                    Measurement::pt(self.paragraph_skip),
                    Measurement::pt(self.paragraph_skip * 0.5),
                    Measurement::pt(0.0),
                )));
                self.vertical_queue.push(Node::penalty(0));
            }
            self.vertical_queue.extend(nodes);
        }

        // Add final eject penalty
        self.vertical_queue.push(Node::penalty(-10_000));

        // Build page layout
        let layout = if self.header_height > 0.0 || self.footer_height > 0.0 {
            PageLayout::with_header_footer(
                self.paper,
                self.margins[0].max(self.margins[1]).max(self.margins[2]).max(self.margins[3]),
                self.header_height,
                self.footer_height,
                self.frame_gap,
            )
        } else {
            self.build_layout()?
        };

        let content_frame_id = layout
            .content_frame_id()
            .ok_or_else(|| BuilderError::Layout("no content frame".to_string()))?;

        // Inject widow/orphan penalties
        PageBuilder::inject_penalties(&mut self.vertical_queue, &self.page_break_settings);

        // Build pages
        let mut page_builder = PageBuilder::new(self.page_break_settings);
        page_builder.enqueue_many(self.vertical_queue);
        let pages = page_builder.build_pages(&layout, content_frame_id);

        // Render to PDF
        let mut pdf = PdfOutputter::new(self.pdf_config);

        // Register fonts
        for (name, entry) in &self.fonts {
            pdf.register_font(name, Arc::clone(&entry.face));
        }

        // Add bookmarks
        for bm in self.bookmarks {
            pdf.add_bookmark(bm);
        }

        // Render pages
        pdf.render_pages(&pages, &layout);

        Ok(pdf.finish()?)
    }

    // -- Internal: paragraph typesetting ------------------------------------

    fn typeset_paragraph(
        &mut self,
        runs: &[TextRun],
    ) -> Result<Vec<Node>, BuilderError> {
        let layout = self.build_layout()?;
        let content_frame_id = layout
            .content_frame_id()
            .ok_or_else(|| BuilderError::Layout("no content frame".to_string()))?;
        let hsize = layout.frame(content_frame_id).width();

        // Build horizontal node list from text runs
        let mut h_nodes = Vec::new();

        // Paragraph indent
        if self.paragraph_indent > 0.0 {
            h_nodes.push(Node::hbox(self.paragraph_indent, 0.0, 0.0));
        }

        for run in runs {
            let font_entry = self.fonts.get(&run.font_name).ok_or_else(|| {
                BuilderError::NoFont(run.font_name.clone())
            })?;
            let face = Arc::clone(&font_entry.face);
            let spec = font_entry.spec.clone();

            let space_width = self.shaper.measure_space(&face, &spec, &self.space_settings);

            let words = split_words(&run.text);
            for (i, word) in words.iter().enumerate() {
                if word.is_empty() {
                    continue;
                }

                if word.chars().all(|c| c.is_whitespace()) {
                    // Emit glue for whitespace
                    if !h_nodes.is_empty() {
                        h_nodes.push(Node::glue(space_width));
                    }
                    continue;
                }

                // Emit glue before word (if not first in paragraph)
                if i > 0 && !h_nodes.is_empty() {
                    // Check if previous was already glue
                    if !h_nodes.last().is_some_and(|n| n.is_glue()) {
                        h_nodes.push(Node::glue(space_width));
                    }
                }

                // Shape the word
                let glyphs = self.shaper.shape(word, &face, &spec);
                let nnode = self.build_nnode(word, &glyphs, &run.font_name, &spec, run.color);
                h_nodes.push(Node::NNode(nnode));
            }
        }

        if h_nodes.is_empty() {
            return Ok(Vec::new());
        }

        // Add parfillskip (infinite stretch glue to fill last line)
        h_nodes.push(Node::hfillglue(Length::zero()));
        h_nodes.push(Node::penalty(-10_000));

        // Pre-hyphenate so we have a single consistent node list for both
        // linebreaking and line building. The linebreaker's internal hyphenation
        // pass modifies its own copy of the node list, making break positions
        // incompatible with the original. By pre-hyphenating we avoid that.
        let h_nodes = hyphenate_nodes(
            &h_nodes,
            &self.language,
            &mut self.hyphenation,
            &mut RustyBuzzShaper::new(),
            &self.fonts,
        );

        // Run line breaker (no callback — nodes are already hyphenated)
        let breaks = linebreak::do_break(
            &h_nodes,
            hsize,
            &self.linebreak_settings,
            None,
        );

        // Package lines into VBoxes
        let v_nodes = self.build_lines(&h_nodes, &breaks, hsize);
        Ok(v_nodes)
    }

    fn build_nnode(
        &self,
        text: &str,
        glyphs: &[GlyphItem],
        font_name: &str,
        spec: &FontSpec,
        color: Option<Color>,
    ) -> NNode {
        let mut width = 0.0;
        let mut height = 0.0_f64;
        let mut depth = 0.0_f64;
        let mut glyph_data = Vec::with_capacity(glyphs.len());

        for g in glyphs {
            width += g.x_advance;
            height = height.max(g.height);
            depth = depth.max(g.depth);
            glyph_data.push(GlyphData {
                gid: g.gid,
                x_advance: g.x_advance,
                y_advance: g.y_advance,
                x_offset: g.x_offset,
                y_offset: g.y_offset,
            });
        }

        let mut nnode = NNode::with_glyphs(text, glyph_data, font_name, spec.size, width, height, depth);
        nnode.color = color;
        nnode.language = self.language.clone();
        nnode
    }

    fn build_lines(
        &self,
        h_nodes: &[Node],
        breaks: &[BreakResult],
        hsize: f64,
    ) -> Vec<Node> {
        let mut v_nodes = Vec::new();
        let mut start = 0;

        for (line_idx, br) in breaks.iter().enumerate() {
            // Collect nodes for this line
            let end = br.position.min(h_nodes.len());
            let mut line_nodes: Vec<Node> = Vec::new();

            // Left indent
            if br.left > 0.0 {
                line_nodes.push(Node::hbox(br.left, 0.0, 0.0));
            }

            // Copy nodes from start..end, skipping leading discardables
            let mut started = false;
            for node in &h_nodes[start..end] {
                if !started && node.is_discardable() {
                    continue;
                }
                started = true;
                line_nodes.push(node.clone());
            }

            // Trim trailing discardables
            while line_nodes.last().is_some_and(|n| n.is_discardable()) {
                line_nodes.pop();
            }

            // Handle discretionary at break point
            if end < h_nodes.len()
                && let Node::Discretionary(d) = &h_nodes[end] {
                    line_nodes.extend(d.prebreak.clone());
                }

            // Right indent
            if br.right > 0.0 {
                line_nodes.push(Node::hbox(br.right, 0.0, 0.0));
            }

            // Compute line dimensions
            let line_height = node::max_node_dim(&line_nodes, node::Dim::Height);
            let line_depth = node::max_node_dim(&line_nodes, node::Dim::Depth);

            let vbox = VBox {
                width: Length::pt(hsize),
                height: line_height,
                depth: line_depth,
                nodes: line_nodes,
                misfit: false,
                explicit: false,
            };

            // Inter-line glue (leading)
            if line_idx > 0 && self.leading > 0.0 {
                v_nodes.push(Node::vglue(Length::new(
                    Measurement::pt(self.leading),
                    Measurement::pt(self.leading * 0.5),
                    Measurement::pt(self.leading * 0.3),
                )));
            }

            v_nodes.push(Node::VBox(vbox));

            // Advance start past the break point + any discardables
            start = end + 1;
            // Skip discardables after break point (consumed by linebreaker)
            while start < h_nodes.len() && h_nodes[start].is_discardable() {
                start += 1;
            }
            // If we broke at a discretionary, skip the postbreak handling
            if end < h_nodes.len() && h_nodes[end].is_discretionary() {
                start = end + 1;
                while start < h_nodes.len() && h_nodes[start].is_discardable() {
                    start += 1;
                }
            }
        }

        v_nodes
    }

    fn build_layout(&self) -> Result<PageLayout, BuilderError> {
        let [top, right, bottom, left] = self.margins;
        let mut layout = PageLayout::new(self.paper);
        let content_id = layout.add_frame("content");
        let constraints = vec![
            FrameConstraint::Left(content_id, left),
            FrameConstraint::Top(content_id, top),
            FrameConstraint::Right(content_id, self.paper.width - right),
            FrameConstraint::Bottom(content_id, self.paper.height - bottom),
        ];
        layout
            .solve(&constraints)
            .map_err(|e| BuilderError::Layout(format!("constraint solver failed: {e:?}")))?;
        Ok(layout)
    }
}

// ---------------------------------------------------------------------------
// Word splitting
// ---------------------------------------------------------------------------

fn split_words(text: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut start = 0;
    let mut in_word = false;

    for (i, c) in text.char_indices() {
        let is_ws = c.is_whitespace();
        if in_word && is_ws {
            words.push(&text[start..i]);
            words.push(&text[i..i + c.len_utf8()]);
            start = i + c.len_utf8();
            in_word = false;
        } else if !in_word && is_ws {
            words.push(&text[i..i + c.len_utf8()]);
            start = i + c.len_utf8();
        } else if !in_word && !is_ws {
            start = i;
            in_word = true;
        }
    }

    if in_word && start < text.len() {
        words.push(&text[start..]);
    }

    words
}

// ---------------------------------------------------------------------------
// Hyphenation callback
// ---------------------------------------------------------------------------

fn hyphenate_nodes(
    nodes: &[Node],
    lang: &str,
    dict: &mut HyphenationDictionary,
    shaper: &mut RustyBuzzShaper,
    fonts: &std::collections::HashMap<String, RegisteredFont>,
) -> Vec<Node> {
    let mut result = Vec::with_capacity(nodes.len());

    for node in nodes {
        if let Node::NNode(nnode) = node {
            let word = &nnode.text;
            if word.chars().count() < dict.min_word
                || !word.chars().all(|c| c.is_alphabetic()) {
                result.push(node.clone());
                continue;
            }

            let segments = dict.hyphenate_word(word, lang);
            if segments.len() <= 1 {
                result.push(node.clone());
                continue;
            }

            // Build discretionary break points between syllables
            let font_entry = match fonts.get(&nnode.font_key) {
                Some(e) => e,
                None => {
                    result.push(node.clone());
                    continue;
                }
            };

            for (i, segment) in segments.iter().enumerate() {
                // Shape this segment
                let glyphs = shaper.shape(segment, &font_entry.face, &font_entry.spec);
                let mut seg_width = 0.0;
                let mut seg_height = 0.0_f64;
                let mut seg_depth = 0.0_f64;
                let mut glyph_data = Vec::with_capacity(glyphs.len());

                for g in &glyphs {
                    seg_width += g.x_advance;
                    seg_height = seg_height.max(g.height);
                    seg_depth = seg_depth.max(g.depth);
                    glyph_data.push(GlyphData {
                        gid: g.gid,
                        x_advance: g.x_advance,
                        y_advance: g.y_advance,
                        x_offset: g.x_offset,
                        y_offset: g.y_offset,
                    });
                }

                let mut seg_nnode = NNode::with_glyphs(
                    segment,
                    glyph_data,
                    &nnode.font_key,
                    nnode.font_size,
                    seg_width,
                    seg_height,
                    seg_depth,
                );
                seg_nnode.color = nnode.color;
                seg_nnode.language = nnode.language.clone();

                result.push(Node::NNode(seg_nnode));

                // Insert discretionary break between segments (not after last)
                if i < segments.len() - 1 {
                    // Shape a hyphen for the prebreak
                    let hyphen_glyphs = shaper.shape("-", &font_entry.face, &font_entry.spec);
                    let mut hw = 0.0;
                    let mut hh = 0.0_f64;
                    let mut hd = 0.0_f64;
                    let mut hglyph_data = Vec::new();
                    for g in &hyphen_glyphs {
                        hw += g.x_advance;
                        hh = hh.max(g.height);
                        hd = hd.max(g.depth);
                        hglyph_data.push(GlyphData {
                            gid: g.gid,
                            x_advance: g.x_advance,
                            y_advance: g.y_advance,
                            x_offset: g.x_offset,
                            y_offset: g.y_offset,
                        });
                    }

                    let hyphen_nnode = NNode::with_glyphs(
                        "-",
                        hglyph_data,
                        &nnode.font_key,
                        nnode.font_size,
                        hw,
                        hh,
                        hd,
                    );

                    result.push(Node::discretionary(
                        vec![Node::NNode(hyphen_nnode)],
                        vec![],
                        vec![],
                    ));
                }
            }
        } else {
            result.push(node.clone());
        }
    }

    result
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn load_any_system_font() -> Option<(Vec<u8>, String)> {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let info = db.faces().next()?;
        let family = info.families.first()?.0.clone();
        let id = info.id;
        let mut data_out: Option<(Vec<u8>, u32)> = None;
        db.with_face_data(id, |data, index| {
            data_out = Some((data.to_vec(), index));
        });
        let (data, _index) = data_out?;
        Some((data, family))
    }

    fn builder_with_font() -> Option<DocumentBuilder> {
        let (data, family) = load_any_system_font()?;
        let mut doc = DocumentBuilder::new(PaperSize::A4);
        let spec = FontSpec {
            family: Some(family),
            size: 12.0,
            ..Default::default()
        };
        doc.load_font_data("body", data, spec).ok()?;
        doc.set_font("body");
        Some(doc)
    }

    // -- Construction --------------------------------------------------------

    #[test]
    fn new_builder() {
        let doc = DocumentBuilder::new(PaperSize::A4);
        assert!((doc.paper.width - 595.276).abs() < 0.01);
    }

    #[test]
    fn set_margins() {
        let mut doc = DocumentBuilder::new(PaperSize::A4);
        doc.set_margins(50.0, 60.0, 70.0, 80.0);
        assert_eq!(doc.margins, [50.0, 60.0, 70.0, 80.0]);
    }

    #[test]
    fn set_page_size() {
        let mut doc = DocumentBuilder::new(PaperSize::A4);
        doc.set_page_size(PaperSize::LETTER);
        assert!((doc.paper.width - 612.0).abs() < 0.01);
    }

    // -- Font loading --------------------------------------------------------

    #[test]
    fn load_font() {
        let (data, family) = match load_any_system_font() {
            Some(v) => v,
            None => return,
        };
        let mut doc = DocumentBuilder::new(PaperSize::A4);
        let spec = FontSpec {
            family: Some(family),
            size: 12.0,
            ..Default::default()
        };
        assert!(doc.load_font_data("body", data, spec).is_ok());
        assert!(doc.fonts.contains_key("body"));
    }

    #[test]
    fn set_font_size() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        doc.set_font_size(24.0);
        assert_eq!(doc.fonts["body"].spec.size, 24.0);
    }

    // -- Text and paragraph --------------------------------------------------

    #[test]
    fn add_text_creates_run() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        doc.add_text("Hello");
        assert_eq!(doc.paragraph_runs.len(), 1);
        assert_eq!(doc.paragraph_runs[0].text, "Hello");
    }

    #[test]
    fn new_paragraph_flushes_runs() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        doc.add_text("Hello, world.");
        assert!(doc.new_paragraph().is_ok());
        assert!(doc.paragraph_runs.is_empty());
        assert!(!doc.vertical_queue.is_empty());
    }

    #[test]
    fn empty_paragraph_is_noop() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        assert!(doc.new_paragraph().is_ok());
        assert!(doc.vertical_queue.is_empty());
    }

    // -- Full render ---------------------------------------------------------

    #[test]
    fn render_hello_world() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        doc.set_title("Hello");
        doc.add_text("Hello, world.");
        let pdf = doc.render().unwrap();
        assert!(pdf.starts_with(b"%PDF"));
        assert!(pdf.len() > 100);
    }

    #[test]
    fn render_multi_paragraph() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        doc.add_text("First paragraph with some text.");
        doc.new_paragraph().unwrap();
        doc.add_text("Second paragraph with more text.");
        doc.new_paragraph().unwrap();
        doc.add_text("Third paragraph wrapping it up.");
        let pdf = doc.render().unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_long_text() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        let text = "To Sherlock Holmes she is always the woman. I have seldom heard \
                    him mention her under any other name. In his eyes she eclipses \
                    and predominates the whole of her sex. It was not that he felt \
                    any emotion akin to love for Irene Adler. All emotions, and that \
                    one particularly, were abhorrent to his cold, precise but \
                    admirably balanced mind. He was, I take it, the most perfect \
                    reasoning and observing machine that the world has seen, but as \
                    a lover he would have placed himself in a false position.";
        doc.add_text(text);
        let pdf = doc.render().unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_with_color() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        doc.set_color(Color::Rgb { r: 1.0, g: 0.0, b: 0.0 });
        doc.add_text("Red text.");
        doc.clear_color();
        doc.add_text(" Normal text.");
        let pdf = doc.render().unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_with_page_break() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        doc.add_text("Page one content.");
        doc.new_paragraph().unwrap();
        doc.add_page_break();
        doc.add_text("Page two content.");
        let pdf = doc.render().unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_with_metadata() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        doc.set_title("My Document")
            .set_author("Test Author")
            .set_subject("Testing")
            .set_compress(false);
        doc.add_text("Content.");
        let pdf = doc.render().unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_with_custom_margins() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        doc.set_margins(36.0, 36.0, 36.0, 36.0);
        doc.add_text("Narrow margins.");
        let pdf = doc.render().unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_with_paragraph_indent() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        doc.set_paragraph_indent(40.0);
        doc.add_text("Indented paragraph.");
        let pdf = doc.render().unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_with_paragraph_skip() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        doc.set_paragraph_skip(12.0);
        doc.add_text("First paragraph.");
        doc.new_paragraph().unwrap();
        doc.add_text("Second paragraph.");
        let pdf = doc.render().unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_with_bookmark() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        doc.add_bookmark("Chapter 1", 0);
        doc.add_text("Chapter content.");
        let pdf = doc.render().unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_empty_document() {
        let doc = DocumentBuilder::new(PaperSize::A4);
        let pdf = doc.render().unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_multiple_fonts() {
        let (data, family) = match load_any_system_font() {
            Some(v) => v,
            None => return,
        };
        let mut doc = DocumentBuilder::new(PaperSize::A4);

        let spec1 = FontSpec {
            family: Some(family.clone()),
            size: 12.0,
            ..Default::default()
        };
        doc.load_font_data("body", data.clone(), spec1).unwrap();

        let spec2 = FontSpec {
            family: Some(family),
            size: 18.0,
            weight: crate::font::FontWeight::BOLD,
            ..Default::default()
        };
        doc.load_font_data("heading", data, spec2).unwrap();

        doc.set_font("heading");
        doc.add_text("Heading Text");
        doc.new_paragraph().unwrap();

        doc.set_font("body");
        doc.add_text("Body text in normal size.");
        let pdf = doc.render().unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_letter_size() {
        let mut doc = match builder_with_font() {
            Some(d) => d,
            None => return,
        };
        doc.set_page_size(PaperSize::LETTER);
        doc.add_text("US Letter content.");
        let pdf = doc.render().unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    // -- Word splitting tests ------------------------------------------------

    #[test]
    fn split_words_simple() {
        let words = split_words("Hello World");
        assert_eq!(words, vec!["Hello", " ", "World"]);
    }

    #[test]
    fn split_words_single() {
        let words = split_words("Hello");
        assert_eq!(words, vec!["Hello"]);
    }

    #[test]
    fn split_words_multiple_spaces() {
        let words = split_words("Hello  World");
        // Two spaces become two separator entries
        assert_eq!(words, vec!["Hello", " ", " ", "World"]);
    }

    #[test]
    fn split_words_empty() {
        let words = split_words("");
        assert!(words.is_empty());
    }

    #[test]
    fn split_words_only_spaces() {
        let words = split_words("   ");
        assert!(words.is_empty() || words.iter().all(|w| w.trim().is_empty()));
    }

    #[test]
    fn split_words_leading_space() {
        let words = split_words(" Hello");
        // Leading space is handled
        assert!(words.contains(&"Hello"));
    }

    #[test]
    fn split_words_trailing_space() {
        let words = split_words("Hello ");
        assert_eq!(words[0], "Hello");
    }
}
