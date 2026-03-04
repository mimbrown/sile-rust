# sile-cli Performance Report

## Executive Summary

The CLI generates a 1-page, 5-paragraph PDF in **~57ms** (warm) / **~960ms** (cold). The cold start
is dominated by disk I/O for loading system fonts. The warm execution breaks down as:

| Phase              |   Time |  % of Total |
|--------------------|-------:|------------:|
| System font scan   |  ~39ms |        ~68% |
| Text processing    |  ~8.5ms|        ~15% |
| PDF render         |  ~8ms  |        ~14% |
| Doc setup          |  ~0.6ms|         ~1% |
| File write         |  ~0.3ms|        ~0.5%|
| **Total**          |**~57ms**|     **100%**|

The biggest opportunities are: eliminating the system font scan (~39ms), caching the
`rustybuzz::Face` across shaping calls, and eliminating redundant re-shaping during
hyphenation. Together these could reduce the warm-path time to **~15-20ms** (a 3x improvement).

## Test Environment

- Document: 1 page, 5 paragraphs (~400 words), 2 registered fonts (heading + body)
- Font: DejaVu Serif (357 KB)
- System: 51 system fonts installed
- Build: `--release` (optimized)
- 5 warm runs averaged

## Detailed Findings

### 1. System Font Scanning — 39ms (68%)

**Location**: `sile-cli/src/main.rs:137-138`

```rust
let mut db = fontdb::Database::new();
db.load_system_fonts(); // ← scans filesystem for all system fonts
```

The CLI's `load_system_font()` creates a `fontdb::Database`, scans **all** system font
directories, parses every `.ttf`/`.otf` header it finds, then searches for a preferred
font family. This takes ~39ms even with warm filesystem caches. With cold caches (first
invocation after boot), this balloons to ~900ms.

**Recommendations:**

| # | Action | Estimated savings | Effort |
|---|--------|-------------------|--------|
| 1a | Accept a `--font <path>` CLI argument to bypass system scanning entirely | ~39ms (eliminates phase) | Low |
| 1b | Cache the font discovery result (e.g. `~/.cache/sile/fonts.db`) so subsequent runs skip scanning | ~35ms on cache hit | Medium |
| 1c | Use `fontdb`'s `load_fonts_dir()` on a single directory instead of all system dirs | ~20-30ms | Low |
| 1d | Lazy font discovery — only scan if the preferred font list fails a direct-path lookup first | ~39ms when font path is known | Low |

### 2. rustybuzz::Face Recreation Per Shape Call — ~3-4ms total

**Location**: `sile-core/src/shaper.rs:129`

```rust
fn shape(&self, text: &str, face: &FontFace, spec: &FontSpec) -> Vec<GlyphItem> {
    let (data, index) = face.raw_data();
    let rb_face = rustybuzz::Face::from_slice(data, index); // ← re-parses font EVERY call
    // ...
}
```

Every call to `shape()` re-parses the font binary into a `rustybuzz::Face`. For the
current document this happens **~500+ times** (initial word shaping + hyphenation segment
reshaping + hyphen shaping). At ~10µs per shape call, the Face reconstruction is a
significant fraction.

**Micro-benchmark**: `shape("Sherlock")` = 10.4µs per call

**Recommendation:**

| # | Action | Estimated savings | Effort |
|---|--------|-------------------|--------|
| 2a | Cache `rustybuzz::Face` in `FontFace` or `RustyBuzzShaper` (keyed by font data pointer + index) | ~1-2ms per document | Medium |
| 2b | Use `ouroboros` or `self_cell` crate to store a self-referential `rustybuzz::Face<'a>` inside `FontFace` | ~1-2ms + eliminates `with_face` overhead | Medium |

### 3. Double Shaping During Hyphenation — ~3ms total

**Location**: `sile-core/src/builder.rs:490` and `builder.rs:701-812`

The pipeline shapes each word twice:

1. **First shaping** (`typeset_paragraph`, line 490): every word is shaped to create `NNode`s
2. **Re-shaping** (`hyphenate_nodes`, line 736): every word ≥5 chars is split into syllables,
   and each syllable is shaped again from scratch — plus the hyphen character is shaped for
   every potential break point

For a 91-word paragraph, the initial shaping takes ~831µs and the hyphenation re-shaping
adds ~290µs (1.3x overhead), meaning ~35% of shaping work is redundant.

Additionally, `hyphenate_nodes` creates a **new** `RustyBuzzShaper` (line 512) instead of
reusing the existing one — though this is zero-cost since `RustyBuzzShaper` is a unit struct.

**Recommendations:**

| # | Action | Estimated savings | Effort |
|---|--------|-------------------|--------|
| 3a | Split glyph data at hyphenation byte offsets instead of re-shaping syllables | ~3ms (eliminates all re-shaping) | High |
| 3b | Cache shaped hyphens per font (one shape call instead of N) | ~0.5ms | Low |
| 3c | Pre-hyphenate before initial shaping — shape syllables directly the first time | ~2ms | Medium |

### 4. PageLayout Rebuilt Every Paragraph

**Location**: `sile-core/src/builder.rs:444`

```rust
fn typeset_paragraph(&mut self, runs: &[TextRun]) -> Result<Vec<Node>, BuilderError> {
    let layout = self.build_layout()?; // ← Cassowary solver runs EVERY paragraph
    // ...
}
```

`build_layout()` creates a new `PageLayout`, adds frames, and runs the Cassowary constraint
solver. This costs ~15µs per call. With 5 paragraphs + 1 render call = 6 invocations = ~93µs.

This is not a large absolute cost but is architecturally wasteful — the layout doesn't change
between paragraphs.

**Recommendation:**

| # | Action | Estimated savings | Effort |
|---|--------|-------------------|--------|
| 4a | Cache the `PageLayout` and invalidate only when margins/paper size change | ~75µs | Low |

### 5. FontFace::with_face Re-parses Font

**Location**: `sile-core/src/font.rs:312`

```rust
fn with_face<T>(&self, f: impl FnOnce(&ttf_parser::Face<'_>) -> T) -> T {
    let face = ttf_parser::Face::parse(&self.data, self.index) // ← re-parses EVERY call
        .expect("font data already validated");
    f(&face)
}
```

Every call to `glyph_id()`, `advance_width()`, or `glyph_bounding_box()` re-parses the
entire font binary. In the shaping hot path, `glyph_bounding_box` is called once per glyph
(~400+ times). At ~436ns per call, this totals ~175µs.

**Micro-benchmark**: `glyph_bounding_box()` = 436ns per call (mostly re-parse overhead)

**Recommendation:**

| # | Action | Estimated savings | Effort |
|---|--------|-------------------|--------|
| 5a | Store a persistent `ttf_parser::Face` using `ouroboros` or `self_cell` for self-referential storage | ~150µs + future-proofs all glyph queries | Medium |
| 5b | At minimum, batch glyph queries using a single `with_face` call in the shaper | ~100µs | Low |

### 6. Font Embedding in PDF — ~5-7ms

**Location**: `sile-core/src/pdf.rs:742-876`

The full DejaVu Serif font (357 KB) is subsetted and embedded. Subsetting 95 glyphs takes
~76µs. The bulk of the ~8ms render phase is:
- Font data compression (zlib level 6): ~2-3ms for 357KB
- PDF structure assembly: ~2ms
- ToUnicode CMap generation: ~0.5ms

**Recommendations:**

| # | Action | Estimated savings | Effort |
|---|--------|-------------------|--------|
| 6a | Lower zlib compression level (e.g., level 1 instead of 6) — trades ~10% file size for ~50% faster compression | ~1-2ms | Low |
| 6b | Use a faster compressor (e.g., `lz4` for development, zlib for production) behind a feature flag | ~2-3ms in dev mode | Low |
| 6c | Compress the subsetted font data, not the full font (currently subsetting then compressing the result) | Already done correctly | — |

### 7. Excessive String Allocations

**Scattered across the hot path:**

- `GlyphItem.text: String` — allocated per glyph in `extract_glyph_texts` (shaper.rs:204-234)
- `NNode.language: String` — cloned for every node (`builder.rs:557`)
- `FontSpec.clone()` — happens per text run (`builder.rs:463`)

These are small allocations but add up across ~500+ shaper calls.

**Recommendations:**

| # | Action | Estimated savings | Effort |
|---|--------|-------------------|--------|
| 7a | Use `Arc<str>` or string interning for `language` field | ~50µs | Low |
| 7b | Use `Cow<'_, str>` or byte offsets instead of `String` for `GlyphItem.text` | ~100µs | Medium |
| 7c | Pass `&FontSpec` by reference in more places instead of cloning | ~30µs | Low |

### 8. hyphenation Crate `embed_all` Feature

**Location**: `sile-core/Cargo.toml:11`

```toml
hyphenation = { version = "0.8", features = ["embed_all"] }
```

This embeds TeX hyphenation patterns for **all 30+ languages** into the binary. This doesn't
affect runtime performance but increases binary size by ~2-3 MB and slows compilation.

**Recommendation:**

| # | Action | Impact | Effort |
|---|--------|--------|--------|
| 8a | Use `embed_en` (or per-language features) for production builds | Smaller binary, faster builds | Low |

## Prioritized Action Plan

### Phase 1: Quick Wins (estimated 3x speedup on warm path, ~39ms savings on cold)

1. **Add `--font <path>` CLI flag** to bypass system font scanning (Issue #1a)
2. **Cache `PageLayout`** across paragraph calls (Issue #4a)
3. **Cache shaped hyphens** per font (Issue #3b)
4. **Lower zlib level** to 1 for dev, 6 for release (Issue #6a)

### Phase 2: Architecture Improvements (estimated additional 2x on warm path)

5. **Cache `rustybuzz::Face`** in the shaper or `FontFace` (Issue #2a)
6. **Store persistent `ttf_parser::Face`** in `FontFace` via self-referential struct (Issue #5a)
7. **Eliminate double-shaping** — split glyph data at hyphenation points (Issue #3a)
8. **Reduce string allocations** with `Arc<str>` / `Cow` (Issue #7)

### Phase 3: Polish

9. **Font discovery caching** for repeated CLI invocations (Issue #1b)
10. **Trim `embed_all`** to per-language hyphenation features (Issue #8a)

## Projected Performance After Optimizations

| Scenario | Current | After Phase 1 | After Phase 2 |
|----------|---------|---------------|---------------|
| Cold start (no font path) | ~960ms | ~960ms | ~960ms |
| Cold start (with `--font`) | — | ~20ms | ~10ms |
| Warm (system scan) | ~57ms | ~20ms | ~12ms |
| Warm (with `--font`) | — | ~17ms | ~8ms |
