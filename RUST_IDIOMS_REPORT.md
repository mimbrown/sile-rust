# Rust Idioms Review: sile-core

This report identifies patterns in `sile-core` that carry over Lua idioms and proposes
Rust-native alternatives. Items are grouped by severity: structural issues first, then
optimizations, then minor polish.

---

## 1. Structural Issues

### 1.1 Duplicated dimension fields across every node struct

Every node struct (`HBox`, `NNode`, `Glue`, `Kern`, `VGlue`, `VKern`, `Penalty`, `VBox`,
`Discretionary`, `Alternative`, `Migrating`) redundantly declares `width: Length`,
`height: Length`, `depth: Length`. This is a Lua-ism — Lua tables all had the same keys.
In Rust, this duplication forces the 18-arm match blocks in `Node::width()`, `height()`,
and `depth()`.

**Proposed fix:** Extract a `Dimensions` struct:

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct Dimensions {
    pub width: Length,
    pub height: Length,
    pub depth: Length,
}
```

Each node struct embeds `pub dims: Dimensions`, and `Node` delegates with a single method:

```rust
impl Node {
    pub fn dims(&self) -> Dimensions {
        match self {
            Node::HBox(n) | Node::ZeroHBox(n) => n.dims,
            Node::NNode(n) => n.dims,
            // ...one arm per struct type, not per enum variant
        }
    }
    pub fn width(&self) -> Length { self.dims().width }
    pub fn height(&self) -> Length { self.dims().height }
    pub fn depth(&self) -> Length { self.dims().depth }
}
```

This cuts ~40 match arms down to ~10 and eliminates a class of copy-paste bugs.

### 1.2 Inflated enum — variant proliferation for behavioral subtypes

The `Node` enum has 18 variants, but many are structurally identical and differ only in
behavior flags:

| Structural type | Enum variants |
|---|---|
| `HBox` | `HBox`, `ZeroHBox` |
| `Glue` | `Glue`, `HFillGlue`, `HssGlue` |
| `VGlue` | `VGlue`, `VFillGlue`, `VssGlue`, `ZeroVGlue` |

In Lua, `node.type = "hfillglue"` was a string tag. In Rust, the enum is doing double
duty as both a type container and a behavior tag. This forces pattern groups like
`Node::Glue(_) | Node::HFillGlue(_) | Node::HssGlue(_)` on nearly every match.

This pattern now affects downstream modules too — `pdf.rs:455` and `pdf.rs:470–473` both
match grouped glue variants when rendering page content, and `pagebuilder.rs:282` does the
same when accumulating page dimensions.

**Proposed fix:** Collapse into fewer variants with a `kind` discriminant:

```rust
pub enum GlueKind { Normal, Fill, Hss }

pub struct Glue {
    pub dims: Dimensions,
    pub kind: GlueKind,
    pub explicit: bool,
}

pub enum Node {
    HBox(HBox),
    NNode(NNode),
    Glue(Glue),      // absorbs HFillGlue, HssGlue
    Kern(Kern),
    VGlue(VGlue),    // absorbs VFillGlue, VssGlue, ZeroVGlue
    VKern(VKern),
    Penalty(Penalty),
    VBox(VBox),
    Unshaped(Unshaped),
    Discretionary(Discretionary),
    Alternative(Alternative),
    Migrating(Migrating),
}
```

This reduces the enum from 18 to 12 variants and eliminates nearly every grouped-arm
match. `is_discardable()`, `is_glue()`, `is_explicit()` become single-arm matches.
Constructors like `Node::hfillglue()` still exist but produce `Glue { kind: Fill, .. }`.

### 1.3 `FontFace::with_face()` re-parses the font on every call

Every glyph query (`glyph_id`, `advance_width`, `glyph_name`, `glyph_bounding_box`) calls
`with_face()`, which calls `ttf_parser::Face::parse()` from scratch. `Face::parse` is
cheap (it reads table directory entries, O(n) on the number of tables) but it's still
redundant work done potentially thousands of times per document.

`RustyBuzzShaper::shape()` (shaper.rs:129) has the same pattern — it calls
`rustybuzz::Face::from_slice()` on every shaping call.

**Proposed fix:** Store the parsed `Face` alongside the raw data. Since `Face<'a>` borrows
from `data`, this is a self-referential struct. Options:

1. **`ouroboros` or `yoke` crate** for safe self-referential storage.
2. **Leak the data into `&'static [u8]`** (acceptable for a long-lived font cache).
3. **Keep the current approach** but document it as a known performance gap and benchmark
   to see if it matters in practice.

Option 1 is the cleanest. The struct becomes:

```rust
#[self_referencing]
pub struct FontFace {
    data: Vec<u8>,
    #[borrows(data)]
    #[covariant]
    face: ttf_parser::Face<'this>,
    // cached metrics...
}
```

### 1.4 String-typed error for `Color::parse`

`Color::parse()` returns `Result<Self, String>`, while the font module correctly uses a
`FontError` enum. String errors lose structure and are impossible to match on
programmatically.

**Proposed fix:** Add a `ColorError` enum (or a shared `ParseError` type):

```rust
#[derive(Debug, Clone)]
pub enum ColorError {
    Empty,
    InvalidHex(String),
    InvalidComponent(String),
    UnknownFormat(String),
}
```

### 1.5 `DocumentBuilder` is a god object

`DocumentBuilder` (builder.rs) has 20+ fields spanning page geometry, font management,
hyphenation, style state, paragraph buffering, layout settings, and PDF config. It does
everything from font loading to text shaping to page breaking to PDF serialization. This
mirrors the Lua architecture where `SILE` was a single global namespace with all state
mixed together.

**Problems:**
- Hard to test any subsystem in isolation (shaping, paragraph building, page layout)
- All state is mutable and interleaved — changing a font triggers cascading updates
- `render()` consumes `self`, preventing incremental document building

**Proposed fix:** Factor into focused types:

```rust
struct ParagraphBuilder<'a> {
    runs: Vec<TextRun>,
    shaper: &'a dyn Shaper,
    hyphenation: &'a mut HyphenationDictionary,
    settings: &'a LinebreakSettings,
    // ...
}

struct DocumentBuilder {
    layout: LayoutConfig,     // paper, margins, frames
    fonts: FontRegistry,      // font loading and lookup
    paragraph: ParagraphBuilder,
    output: OutputConfig,     // PDF metadata, compression
}
```

Each piece becomes independently testable and reusable.

### 1.6 Duplicated constants across modules

`AWFUL_BAD`, `INF_BAD`, and `EJECT_PENALTY` are defined identically in both `linebreak.rs`
and `pagebuilder.rs`:

```rust
// linebreak.rs:11-13
const AWFUL_BAD: i64 = 1_073_741_823;
const INF_BAD: i64 = 10_000;
const EJECT_PENALTY: i32 = -10_000;

// pagebuilder.rs:8-11
const INF_BAD: i64 = 10_000;
const EJECT_PENALTY: i32 = -10_000;
const INF_PENALTY: i32 = 10_000;
const AWFUL_BAD: i64 = 1_073_741_823;
```

**Proposed fix:** Define once in a shared module (e.g., `lib.rs` or a new `constants.rs`):

```rust
pub mod constants {
    pub const AWFUL_BAD: i64 = 1_073_741_823;
    pub const INF_BAD: i64 = 10_000;
    pub const EJECT_PENALTY: i32 = -10_000;
    pub const INF_PENALTY: i32 = 10_000;
}
```

### 1.7 Duplicated badness computation

`rate_badness` in `linebreak.rs:168` and `v_badness` in `pagebuilder.rs:85` implement the
same formula (`100 * |shortfall/spring|^3`, capped at `INF_BAD`). The only difference is
that `v_badness` has a special case for near-zero shortfall with zero stretch.

**Proposed fix:** Extract a shared `badness(shortfall, spring)` function, and have
`v_badness` add its near-zero special case on top.

---

## 2. Idiomatic Rust Patterns

### 2.1 Implement `std::iter::Sum` for `Length`

The codebase repeats this fold pattern 6+ times:

```rust
nodes.iter().map(|n| n.width()).fold(Length::zero(), |a, b| a + b)
```

(`sum_widths`, `prebreak_width`, `postbreak_width`, `replacement_width`, `Node::width()` folding, `VBox::append`)

Implementing `Sum` allows:

```rust
impl std::iter::Sum for Length {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::zero(), |a, b| a + b)
    }
}

// Usage:
let total: Length = nodes.iter().map(|n| n.width()).sum();
```

### 2.2 Implement compound assignment operators

`Length` and `Measurement` implement `Add`, `Sub`, `Mul`, `Div` but not `AddAssign`,
`SubAssign`, etc. This forces patterns like:

```rust
self.height = self.height + h + d;  // node.rs:717
```

Instead of:

```rust
self.height += h + d;
```

The linebreak module uses the workaround `self.active_width += w;` which means `AddAssign`
is partially implemented or uses `Add` + assignment. This should be consistent.

**Proposed fix:** Derive or implement `AddAssign`, `SubAssign` for both types.

### 2.3 `to_text()` should return `Cow<'_, str>`

`Node::to_text()` returns `String`, but most branches return constant strings
(`"hbox"`, `" "`, `"(!)"`) or clone an existing string. Using `Cow` avoids allocation
for the constant cases:

```rust
pub fn to_text(&self) -> Cow<'_, str> {
    match self {
        Node::HBox(_) | Node::ZeroHBox(_) => Cow::Borrowed("hbox"),
        Node::NNode(n) => Cow::Borrowed(&n.text),
        Node::Unshaped(n) => Cow::Borrowed(&n.text),
        Node::Glue(_) | Node::Kern(_) | .. => Cow::Borrowed(" "),
        Node::Penalty(_) => Cow::Borrowed("(!)"),
        // ...
    }
}
```

This also eliminates the `n.text.clone()` calls.

### 2.4 `node_type()` → derive or remove

`node_type()` returns a `&'static str` matching the Lua type tag. In Rust, callers
typically match on the enum directly rather than comparing strings. If the string form is
needed for serialization/debugging, consider deriving it with `strum::AsRefStr` or
`strum::Display`, which keeps the mapping in sync with the enum automatically and
eliminates the manual 18-arm match.

If `node_type()` is only used for debugging, it's already covered by `Debug`.

### 2.5 Replace `is_*` method proliferation with enum matching

Node has 14 `is_*` methods (`is_hbox`, `is_glue`, `is_vglue`, `is_kern`, `is_penalty`,
etc.). In Rust, callers use `matches!()` or pattern matching directly — there's no need
for these to live on the type. They're a Lua-ism where `node:is_box()` was the only way
to do type checks.

**Recommendation:** Keep `is_box()` and `is_discardable()` (semantic queries that span
multiple variants). Remove the rest — callers should use `matches!(node, Node::Glue(_))`
directly. This reduces the API surface and makes the enum the single source of truth.

### 2.6 `Discretionary` width methods duplicate `sum_widths`

`prebreak_width()`, `postbreak_width()`, and `replacement_width()` each inline the same
fold. They should call the existing `sum_widths()` helper:

```rust
pub fn prebreak_width(&self) -> Length { sum_widths(&self.prebreak) }
pub fn postbreak_width(&self) -> Length { sum_widths(&self.postbreak) }
pub fn replacement_width(&self) -> Length { sum_widths(&self.replacement) }
```

### 2.7 Duplicated GlyphItem → GlyphData + NNode conversion

`builder.rs:build_nnode` (line 529) and the loop inside `hyphenate_nodes` (line 734)
perform the exact same conversion: iterate glyphs, accumulate width/height/depth, map
`GlyphItem` fields to `GlyphData`, construct an `NNode`. This is ~25 lines duplicated
verbatim.

**Proposed fix:** Extract into a shared helper:

```rust
fn glyphs_to_nnode(
    text: &str,
    glyphs: &[GlyphItem],
    font_key: &str,
    font_size: f64,
    color: Option<Color>,
    language: &str,
) -> NNode { ... }
```

### 2.8 `render()` duplicates paragraph-flushing logic

`DocumentBuilder::render()` (builder.rs:376–389) duplicates the paragraph-flushing logic
from `new_paragraph()` (builder.rs:294–308). Both check `paragraph_runs.is_empty()`, call
`typeset_paragraph`, add inter-paragraph skip, and extend the vertical queue.

**Proposed fix:** Have `render()` call `self.new_paragraph()?` at the top, then proceed
with the flush-is-noop guarantee.

### 2.9 `show_glyphs` uses unnamed tuple fields

`PdfOutputter::show_glyphs` (pdf.rs:320) takes `&[(u16, f64, f64, f64, f64)]` for glyphs.
Five unnamed `f64`s are opaque and error-prone. The existing `GlyphData` struct has named
fields for this exact data.

**Proposed fix:** Use `&[GlyphData]` directly:

```rust
pub fn show_glyphs(&mut self, x: f64, y: f64, font_key: &str, font_size: f64, glyphs: &[GlyphData])
```

### 2.10 `split_words` reimplements iterator-based splitting

`split_words` (builder.rs:669) manually tracks indices with `char_indices`, a boolean
`in_word` flag, and byte offset arithmetic. The standard library's `str::split_inclusive`
or the `unicode-segmentation` crate's `UnicodeSegmentation::split_word_bounds` handle this
more idiomatically and correctly for Unicode.

```rust
// Current: 25-line manual char_indices loop
fn split_words(text: &str) -> Vec<&str> { ... }

// Idiomatic alternative:
use unicode_segmentation::UnicodeSegmentation;
fn split_words(text: &str) -> Vec<&str> {
    text.split_word_bounds().collect()
}
```

---

## 3. Performance Optimizations

### 3.1 Named color lookup: O(n) → O(1) with `phf`

`NAMED_COLORS` is a 148-entry static slice searched linearly on every `Color::parse()`
call with a named color. The `phf` crate generates a perfect hash map at compile time:

```rust
use phf::phf_map;

static NAMED_COLORS: phf::Map<&'static str, [u8; 3]> = phf_map! {
    "aliceblue" => [240, 248, 255],
    "antiquewhite" => [250, 235, 215],
    // ...
};

// Lookup: O(1)
if let Some(&[r, g, b]) = NAMED_COLORS.get(lower.as_str()) { ... }
```

This is a measurable improvement if color parsing happens frequently (e.g., per-element
style resolution).

### 3.2 `FontSpec::cache_key()` allocates on every call

`cache_key()` uses `format!()` to build a `String`, which allocates. Since the cache key
is only used for HashMap lookup, consider:

1. **Hashing directly** — implement `Hash` for `FontSpec` (excluding language/script) and
   use `FontSpec` as the HashMap key, or
2. **Use a `SmallString`/stack buffer** — for cache keys under ~128 bytes, a stack-allocated
   buffer avoids the heap trip.

Option 1 is simplest:

```rust
#[derive(Hash, PartialEq, Eq)]
struct FontCacheKey {
    family: Option<String>,
    size_bits: u64,  // f64::to_bits() for exact hash
    weight: FontWeight,
    style: FontStyle,
    features: String,
    variations: String,
    direction: Direction,
    filename: Option<String>,
}
```

### 3.3 `find_keyword` reimplements `str::find`

```rust
// Current: manual byte-window sliding
fn find_keyword(haystack: &str, needle: &str) -> Option<usize> {
    haystack.as_bytes().windows(needle.len()).position(|w| w == needle.as_bytes())
}

// Equivalent stdlib call:
haystack.find(needle)
```

`str::find` is optimized (uses SIMD on many platforms) and should be preferred.

### 3.4 `VBox::append` allocates a `Vec` for the single-node case

```rust
let nodes_to_add: Vec<Node> = match node {
    Node::VBox(vb) if ... => vb.nodes,
    _ => vec![node],  // heap allocation for one element
};
```

**Proposed fix:** Use `Either` from `itertools` or `smallvec`:

```rust
use smallvec::SmallVec;
let nodes_to_add: SmallVec<[Node; 1]> = match node {
    Node::VBox(vb) if ... => SmallVec::from_vec(vb.nodes),
    _ => SmallVec::from_buf([node]),
};
```

Or refactor to iterate differently for each case without collecting.

### 3.5 `PageBuilder.queue.remove(0)` is O(n)

`pagebuilder.rs:372` and `:441` trim leading discardable nodes with:

```rust
while self.queue.first().is_some_and(|n| n.is_discardable()) {
    self.queue.remove(0);
}
```

`Vec::remove(0)` shifts all remaining elements left — O(n) per removal, O(n²) worst case
for a long queue.

**Proposed fix:** Use `VecDeque` for the queue, which supports O(1) `pop_front()`. Or
batch the trimming:

```rust
let skip = self.queue.iter().take_while(|n| n.is_discardable()).count();
self.queue.drain(..skip);
```

### 3.6 `build_pages` clones all result pages

`PageBuilder::build_pages` (pagebuilder.rs:390) does:

```rust
self.pages.extend(result_pages.clone());
result_pages
```

This clones every page's node data just to store a second copy. Since pages contain
`Vec<Node>` with shaped glyph data, this is a significant allocation.

**Proposed fix:** Return by reference, or let callers choose whether to store:

```rust
// Option A: don't double-store
pub fn build_pages(...) -> Vec<Page> {
    // ...
    self.pages.extend_from_slice(&result_pages);
    // or just: return and let caller own
}
```

If callers always consume the return value, remove the internal `self.pages` storage
entirely.

### 3.7 `extract_glyph_texts` builds a HashMap for sorted data

`shaper.rs:204` builds a `HashMap<u32, u32>` mapping each cluster to its end offset, but
clusters from the shaper output are already sorted. A simple linear scan with
`windows(2)` or a sorted `Vec` lookup would avoid the hash overhead:

```rust
fn extract_glyph_texts(text: &str, infos: &[GlyphInfo]) -> Vec<String> {
    let text_len = text.len() as u32;
    infos.iter().enumerate().map(|(i, info)| {
        let start = info.cluster as usize;
        let end = infos.get(i + 1)
            .map(|next| next.cluster as usize)
            .unwrap_or(text.len());
        text[start.min(text.len())..end.min(text.len())].to_string()
    }).collect()
}
```

Note: this only works when clusters are monotonically increasing (LTR). For RTL text,
clusters are decreasing, so the current HashMap approach is correct but overkill. A
sort + binary search would be more efficient.

### 3.8 `PageLayout` rebuilt on every paragraph

`DocumentBuilder::typeset_paragraph` (builder.rs:444) calls `self.build_layout()` which
creates a new `PageLayout`, allocates `Solver`, adds constraints, and solves — every time
a paragraph is typeset. Since the layout is invariant during a document build, it should
be built once and cached.

**Proposed fix:** Build the layout lazily on first use and cache it:

```rust
fn layout(&mut self) -> Result<&PageLayout, BuilderError> {
    if self.cached_layout.is_none() {
        self.cached_layout = Some(self.build_layout()?);
    }
    Ok(self.cached_layout.as_ref().unwrap())
}
```

Invalidate only when page geometry changes (`set_page_size`, `set_margins`, etc.).

---

## 4. Type Safety Improvements

### 4.1 Measurement arithmetic panics on relative units

`Measurement::Add` and `Sub` use `assert!` to prevent cross-unit arithmetic on relative
measurements. This is a runtime panic hidden inside an operator — callers have no way to
handle the error.

Since Rust's `Add` trait can't return `Result`, the cleanest fix is a **newtype split**:

```rust
pub struct AbsoluteMeasurement { amount: f64, unit: AbsoluteUnit }
pub struct RelativeMeasurement { amount: f64, unit: RelativeUnit }
```

Arithmetic is only implemented on `AbsoluteMeasurement`. Conversion from
`RelativeMeasurement` requires an explicit `resolve(context)` call that returns
`AbsoluteMeasurement`. This moves the error from runtime to compile time.

This is a larger refactor but eliminates an entire class of panics.

### 4.2 `unwrap_or(0.0)` silently drops relative units across the codebase

The original report noted this in `max_node_dim`. With the new modules, the pattern is
far more widespread:

| Location | Code |
|---|---|
| `frame.rs:119` | `node.height().to_pt().unwrap_or(0.0)` in `content_height()` |
| `pagebuilder.rs:277–278` | `node.height().to_pt().unwrap_or(0.0)` in `find_break()` |
| `pagebuilder.rs:283–285` | `g.height.length.to_pt().unwrap_or(0.0)` for vglue stretch/shrink |
| `pdf.rs:444–445` | `vbox.height.to_pt().unwrap_or(0.0)` in `render_page_content()` |
| `pdf.rs:456–459` | glue/kern width `to_pt().unwrap_or(0.0)` |

Every one of these silently treats a relative-unit dimension as 0, producing incorrect
layouts. At minimum, these should `debug_assert!` that the unit is absolute. Ideally, the
type system (4.1) should make this impossible.

### 4.3 `FontSpec` string fields should be typed

`features`, `variations`, `language`, `script` are bare `String` fields. These have
well-defined formats:

- **features**: OpenType feature tags (e.g., `"smcp"`, `"liga"`) — should be a
  `Vec<OTFeature>` where `OTFeature` is a validated 4-byte tag + value.
- **language**: BCP 47 / OpenType language tag — could be an enum or validated newtype.
- **script**: ISO 15924 / OpenType script tag — same treatment.

This prevents invalid data from propagating silently through the system.

### 4.4 `begin_page`/`end_page` state machine uses panics

`PdfOutputter` uses an imperative page API with `begin_page`/`end_page` (pdf.rs:262–282).
Invalid transitions panic:

```rust
pub fn begin_page(&mut self, width: f64, height: f64) {
    assert!(self.current.is_none(), "end_page() not called before begin_page()");
    // ...
}
```

And `self.current.as_mut().expect("no current page")` appears ~15 times throughout methods
like `set_font`, `set_color`, `show_glyphs`, `draw_rule`, etc.

**Proposed fix:** Use a typestate pattern or return `Result`:

```rust
// Typestate approach:
struct PdfOutputter<S> { ... }
struct NeedPage;
struct HasPage;

impl PdfOutputter<NeedPage> {
    fn begin_page(self, w: f64, h: f64) -> PdfOutputter<HasPage> { ... }
}
impl PdfOutputter<HasPage> {
    fn show_glyphs(&mut self, ...) { ... }
    fn end_page(self) -> PdfOutputter<NeedPage> { ... }
}
```

This eliminates the entire class of "no current page" panics at compile time.

### 4.5 Frame lookup by magic string

`PageLayout::content_frame()` and `content_frame_id()` (frame.rs:538–547) search frames
by `f.name == "content"`. This is a magic string that could easily be misspelled with no
compile-time error.

**Proposed fix:** Use a typed enum or a distinguished `content_frame` field:

```rust
pub enum FrameRole { Header, Content, Footer, Custom(String) }

// Or simply:
pub struct PageLayout {
    content_frame_id: Option<FrameId>,  // set at construction
    // ...
}
```

### 4.6 `PdfError` wraps strings, not errors

`PdfError` variants all contain `String` (pdf.rs:18–22):

```rust
pub enum PdfError {
    Font(String),
    Image(String),
    Io(String),
}
```

This loses the original error type and its backtrace. In Rust, error types should wrap
source errors with `#[source]` / `#[from]`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("PDF font error: {0}")]
    Font(#[from] FontError),
    #[error("PDF image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("PDF I/O error: {0}")]
    Io(#[from] std::io::Error),
}
```

---

## 5. Module-Specific Issues

### 5.1 `linebreak.rs`: `LineBreaker` has 30+ fields

`LineBreaker` (linebreak.rs:211) stores 30+ fields, many of which are transient state for
a single `try_break` call (`no_break_yet`, `prev_prev_r`, `prev_r`, `r`, `old_l`,
`line_width`, `badness`, `fit_class_val`, `artificial_demerits`, `last_ratio`).

This is a direct port of TeX's static variables. In Rust, these should be local variables
or a `TryBreakState` struct passed through the call chain:

```rust
struct TryBreakState {
    no_break_yet: bool,
    prev_prev_r: usize,
    prev_r: usize,
    r: usize,
    old_l: i32,
    // ...
}
```

This makes the data flow explicit and prevents accidental reads of stale state.

### 5.2 `linebreak.rs`: `#[allow(dead_code)]` on struct fields

`ActiveNode::ratio` and `ActiveNode::fitness` (linebreak.rs:112,115) are marked
`#[allow(dead_code)]`. They are written but never read. Either use them (e.g., for
justification ratio reporting) or remove them to eliminate dead writes.

### 5.3 `pagebuilder.rs`: `build_pages` and `build_pages_multi_frame` duplication

`build_pages` (pagebuilder.rs:334) and `build_pages_multi_frame` (pagebuilder.rs:396)
share ~30 lines of identical logic: splitting at break points, trimming discardables
from ends, creating `Page` objects. The only difference is that multi-frame follows
the frame chain.

**Proposed fix:** Extract the shared splitting logic:

```rust
fn split_at_break(&mut self, break_idx: usize) -> Vec<Node> {
    let (page_nodes, remaining) = if break_idx + 1 >= self.queue.len() {
        (std::mem::take(&mut self.queue), Vec::new())
    } else {
        let remaining = self.queue.split_off(break_idx + 1);
        (std::mem::take(&mut self.queue), remaining)
    };
    let mut content = page_nodes;
    while content.last().is_some_and(|n| n.is_discardable()) {
        content.pop();
    }
    self.queue = remaining;
    self.trim_leading_discardables();
    content
}
```

### 5.4 `pdf.rs`: `track_glyph` and `track_glyph_on_font` are near-duplicates

`track_glyph` (pdf.rs:192) takes `font_key`, `gid`, and `Option<char>`, while
`track_glyph_on_font` (pdf.rs:354) takes `font_key` and `gid`. The latter is just
`track_glyph(key, gid, None)`. Remove the duplicate.

### 5.5 `pdf.rs`: `write_font` is a 130-line monolith

`write_font` (pdf.rs:742) handles Type0 font creation, CIDFont, font descriptor, font
embedding, CIDToGIDMap, and ToUnicode CMap all in one function. Each of these is an
independent PDF structure.

**Proposed fix:** Split into focused helpers:

```rust
fn write_type0_font(...) { ... }
fn write_cid_font(...) { ... }
fn write_font_descriptor(...) { ... }
fn write_cid_to_gid_map(...) { ... }
fn write_tounicode_cmap(...) { ... }
```

### 5.6 `hyphenation.rs`: `hyphenate_word` takes `&mut self` unnecessarily

`hyphenate_word` (hyphenation.rs:40) takes `&mut self` only because it calls
`self.load_language()` as a side effect. This conflates "ensure dictionary is loaded" with
"hyphenate a word". In Rust, methods should take the minimal borrow.

**Proposed fix:** Split into two operations:

```rust
pub fn ensure_loaded(&mut self, lang: &str) -> bool { ... }
pub fn hyphenate_word(&self, word: &str, lang: &str) -> Vec<String> {
    // Panics or returns unchanged if language not loaded
}
```

Or use the entry API internally so `hyphenate_word` only needs `&self` if the dictionary
is already loaded.

### 5.7 `builder.rs`: `set_font_size` clones to work around borrow checker

```rust
// builder.rs:219-223
pub fn set_font_size(&mut self, size: f64) -> &mut Self {
    if let Some(ref name) = self.current_font.clone()
        && let Some(entry) = self.fonts.get_mut(name) {
            entry.spec.size = size;
        }
    self
}
```

The `.clone()` is needed because `current_font` and `fonts` are both fields of `self`.
This is a sign that these should be separate types (see 1.5), but a quick fix is:

```rust
pub fn set_font_size(&mut self, size: f64) -> &mut Self {
    let name = self.current_font.as_deref().map(str::to_owned);
    if let Some(name) = name {
        if let Some(entry) = self.fonts.get_mut(&name) {
            entry.spec.size = size;
        }
    }
    self
}
```

Or better, use a `FrameId`-style font handle instead of a string key.

### 5.8 `frame.rs`: Redundant solver reset

```rust
// frame.rs:304-305
self.solver.reset();
self.solver = Solver::new();
```

`Solver::new()` creates a fresh solver, making the preceding `reset()` a no-op. Remove
the dead call.

---

## 6. Minor Polish

### 6.1 Missing `Default` derive for `Dim`

`Dim` (the dimension selector enum) doesn't derive `Default`, `Clone`, or `Copy`. It
should at minimum derive `Clone, Copy` since it's a fieldless enum used by value.

### 6.2 Inconsistent Display implementations

`Glue`, `Kern`, `VGlue`, `VKern` don't implement `Display` — their formatting is handled
in `Node::Display`. This means you can't format them independently. For consistency, each
struct should have its own `Display` impl, and `Node::Display` should delegate.

### 6.3 `Discretionary::clone_as_postbreak` uses assert

```rust
assert!(self.used, "Cannot clone a non-used discretionary as postbreak");
```

This should return `Result` or `Option` — the caller may want to handle this gracefully
rather than panicking.

### 6.4 Consider `#[non_exhaustive]` on public enums

If `Node`, `Unit`, `Color` etc. are intended to be extensible in future phases (new node
types, new units), marking them `#[non_exhaustive]` prevents downstream code from writing
exhaustive matches that would break on additions.

### 6.5 `format_f64` allocates a String for Display

```rust
pub(crate) fn format_f64(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 {
        format!("{:.0}", v)
    } else {
        format!("{}", v)
    }
}
```

This is called from `Measurement::Display`, which already has a formatter. Write directly:

```rust
impl std::fmt::Display for Measurement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.amount.is_finite() && self.amount.fract() == 0.0 {
            write!(f, "{:.0}{}", self.amount, self.unit)
        } else {
            write!(f, "{}{}", self.amount, self.unit)
        }
    }
}
```

This eliminates the intermediate `String` allocation.

### 6.6 Duplicated `load_system_font_and_spec` test helper

The function `load_system_font_and_spec()` is copy-pasted across `shaper.rs`, `svg.rs`,
and `builder.rs` test modules (with minor variations). It should live in a shared test
utilities module:

```rust
// In a #[cfg(test)] module in lib.rs or a test_utils.rs:
pub(crate) fn load_system_font_and_spec() -> Option<(FontFace, FontSpec)> { ... }
```

### 6.7 `RustyBuzzShaper` is a unit struct with unnecessary `new()`

`RustyBuzzShaper` has no fields but provides `new()` and a `Default` impl that calls
`new()`. Since it's stateless, it could be a zero-cost abstraction or just use
`Default::default()` everywhere. The `new()` is unnecessary boilerplate.

---

## Summary

| Category | Item | Impact | Effort |
|---|---|---|---|
| Structural | 1.1 Extract `Dimensions` struct | High | Medium |
| Structural | 1.2 Collapse glue/vglue variants | High | Medium |
| Structural | 1.3 Cache parsed `FontFace` / rustybuzz Face | Medium | Medium |
| Structural | 1.4 Typed `ColorError` | Low | Low |
| Structural | 1.5 Split `DocumentBuilder` god object | High | High |
| Structural | 1.6 Shared constants module | Low | Low |
| Structural | 1.7 Shared badness computation | Low | Low |
| Idiom | 2.1 Implement `Sum` for `Length` | Low | Low |
| Idiom | 2.2 Compound assignment ops | Low | Low |
| Idiom | 2.3 `Cow<str>` for `to_text()` | Low | Low |
| Idiom | 2.4 Derive `node_type` or remove | Low | Low |
| Idiom | 2.5 Remove `is_*` methods | Low | Low |
| Idiom | 2.6 Reuse `sum_widths` | Low | Low |
| Idiom | 2.7 Extract glyph→NNode helper | Medium | Low |
| Idiom | 2.8 Deduplicate paragraph flush in `render()` | Low | Low |
| Idiom | 2.9 Use `GlyphData` in `show_glyphs` | Low | Low |
| Idiom | 2.10 Use `unicode-segmentation` for word splitting | Low | Low |
| Perf | 3.1 `phf` for named colors | Medium | Low |
| Perf | 3.2 Hash-based font cache key | Low | Medium |
| Perf | 3.3 Use `str::find` | Low | Low |
| Perf | 3.4 Avoid Vec alloc in `append` | Low | Low |
| Perf | 3.5 `VecDeque` for page builder queue | Medium | Low |
| Perf | 3.6 Don't clone pages in `build_pages` | Medium | Low |
| Perf | 3.7 Avoid HashMap in `extract_glyph_texts` | Low | Low |
| Perf | 3.8 Cache `PageLayout` across paragraphs | Medium | Low |
| Safety | 4.1 Absolute/Relative measurement split | High | High |
| Safety | 4.2 Audit `unwrap_or(0.0)` across all modules | Medium | Low |
| Safety | 4.3 Typed FontSpec fields | Medium | Medium |
| Safety | 4.4 Typestate for PDF page lifecycle | Medium | Medium |
| Safety | 4.5 Typed frame roles instead of magic strings | Low | Low |
| Safety | 4.6 Wrap source errors in `PdfError` | Low | Low |
| Module | 5.1 Factor `LineBreaker` transient state | Medium | Medium |
| Module | 5.2 Remove dead `ActiveNode` fields | Low | Low |
| Module | 5.3 Deduplicate page-splitting logic | Medium | Low |
| Module | 5.4 Remove `track_glyph_on_font` duplicate | Low | Low |
| Module | 5.5 Split `write_font` into focused helpers | Low | Medium |
| Module | 5.6 Split `hyphenate_word` mutability | Low | Low |
| Module | 5.7 Fix `set_font_size` borrow workaround | Low | Low |
| Module | 5.8 Remove redundant solver reset | Low | Low |
| Polish | 6.1–6.7 Various | Low | Low |

**Recommended priority order:**

1. **Quick wins** (< 1 hour each): 1.6, 1.7, 2.7, 2.8, 3.5, 3.6, 3.8, 5.2, 5.4, 5.8
2. **High-impact refactors**: 1.1 → 1.2 (eliminate ~60% of match arm boilerplate)
3. **Correctness**: 4.2 (audit all `unwrap_or(0.0)` sites — there are now 10+)
4. **Architecture**: 1.5 (DocumentBuilder split — unlocks isolated testing)
5. **Safety**: 4.4 (typestate PDF lifecycle — eliminates all "no current page" panics)
6. **Larger follow-ups**: 1.3, 4.1
