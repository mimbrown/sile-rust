# Native HarfBuzz Support Plan

## Motivation

rustybuzz is excellent for v1 — pure Rust, WASM-compatible, 98.6% HarfBuzz test pass rate. But in
native environments, C HarfBuzz offers:

1. **Graphite shaping** — required for Awami Nastaliq, Scheherazade New, and other SIL fonts that use
   Graphite tables for complex positioning that OpenType cannot express
2. **AAT shaping** — Apple Advanced Typography for macOS system fonts
3. **Performance** — 1.5–2× faster; irrelevant for single documents but matters for batch/server use
4. **100% shaping fidelity** — the 31 failing rustybuzz edge cases disappear
5. **Faster feature adoption** — new OpenType/HarfBuzz features land in C HarfBuzz first

The goal: use HarfBuzz on native targets, keep rustybuzz for WASM, with zero impact on the rest of
the codebase thanks to the existing `Shaper` trait.

---

## Current State

The `Shaper` trait (`sile-core/src/shaper.rs:61`) already provides perfect abstraction:

```rust
pub trait Shaper {
    fn shape(&self, text: &str, face: &FontFace, spec: &FontSpec) -> Vec<GlyphItem>;
    fn measure_char(…) -> (CharMetrics, bool);  // default impl
    fn measure_space(…) -> Length;               // default impl
}
```

`RustyBuzzShaper` is the sole implementation. It:
- Creates a `rustybuzz::Face` from raw font bytes on every `shape()` call
- Sets direction, script, language on a `UnicodeBuffer`
- Parses features from the comma-separated `spec.features` string
- Calls `rustybuzz::shape()` and converts results to `Vec<GlyphItem>`
- Queries `FontFace::glyph_bounding_box()` (via ttf-parser) for height/depth

**Note:** Font variations are declared in `FontSpec` but not yet wired to rustybuzz.

---

## Crate Selection

### Recommended: `harfbuzz-sys` (servo/rust-harfbuzz) with a thin safe wrapper

| Option | Pros | Cons |
|--------|------|------|
| `harfbuzz` 0.6 (servo high-level) | Safe API | No graphite feature flag; limited API surface; doesn't expose variations |
| `harfbuzz_rs` 2.0 (harfbuzz org) | Safe, idiomatic | Alpha state; passively maintained; no graphite flag; builds its own HarfBuzz |
| `harfbuzz-sys` 0.6 (servo raw) | Full C API access; bundled build option | Unsafe FFI; need our own safe wrapper |
| System pkg-config | Graphite if system has it | Fragile; user must install the right build |
| `tectonic_bridge_harfbuzz` | Has graphite support | Tectonic-specific; heavy dependency |

**Decision: `harfbuzz-sys` with `bundled` feature + custom graphite2 build integration.**

We write a thin unsafe wrapper (~200 lines) that maps exactly to the operations `RustyBuzzShaper`
already performs. This avoids depending on third-party safe wrappers that may not expose what we
need (variations, graphite). The servo `harfbuzz-sys` crate is well-maintained (used by Firefox)
and supports bundled builds.

For Graphite2, we'll need to either:
- Fork `harfbuzz-sys` to add a `graphite` feature that passes `-DHB_HAVE_GRAPHITE2=ON` during the
  bundled build and links `graphite2` (vendored or system)
- Or maintain our own `-sys` crate that wraps the HarfBuzz + Graphite2 build

The fork approach is simpler initially. The build change is small — HarfBuzz's CMake/meson already
supports `HB_HAVE_GRAPHITE2`; we just need to enable it and link the graphite2 library.

---

## Architecture

### Feature flags in `sile-core/Cargo.toml`

```toml
[features]
default = []
harfbuzz-native = ["harfbuzz-sys"]   # C HarfBuzz, native only
graphite = ["harfbuzz-native"]       # implies harfbuzz-native + Graphite2

[dependencies]
rustybuzz = "0.20"                   # always available as WASM fallback
harfbuzz-sys = { version = "0.6", features = ["bundled"], optional = true }
```

### Shaper selection

```
┌─────────────────────────────────────────┐
│            Shaper trait                  │
│  shape() / measure_char() / measure_space() │
├──────────────────┬──────────────────────┤
│ RustyBuzzShaper  │  HarfBuzzShaper      │
│ (always built)   │  (cfg harfbuzz-native)│
│ used for WASM    │  used for native     │
└──────────────────┴──────────────────────┘
```

Selection at runtime or compile-time:

```rust
pub fn default_shaper() -> Box<dyn Shaper> {
    #[cfg(feature = "harfbuzz-native")]
    { Box::new(HarfBuzzShaper::new()) }

    #[cfg(not(feature = "harfbuzz-native"))]
    { Box::new(RustyBuzzShaper::new()) }
}
```

The WASM build never enables `harfbuzz-native` (it can't link C), so it always gets `RustyBuzzShaper`.

---

## Implementation Plan

### Step 1: HarfBuzz FFI wrapper module

Create `sile-core/src/harfbuzz_ffi.rs` (gated behind `#[cfg(feature = "harfbuzz-native")]`).

This module wraps the raw `harfbuzz-sys` functions into safe Rust types:

```rust
// Owned wrapper around hb_blob_t + hb_face_t + hb_font_t
pub(crate) struct HbFont { … }

impl HbFont {
    /// Create from raw font bytes and face index
    pub fn from_bytes(data: &[u8], index: u32) -> Option<Self>;

    /// Set font size (scale) in 26.6 fixed-point
    pub fn set_scale(&mut self, x_scale: i32, y_scale: i32);

    /// Set font variations
    pub fn set_variations(&mut self, variations: &[(Tag, f32)]);
}

impl Drop for HbFont {
    // hb_font_destroy, hb_face_destroy, hb_blob_destroy
}

// Owned wrapper around hb_buffer_t
pub(crate) struct HbBuffer { … }

impl HbBuffer {
    pub fn new() -> Self;
    pub fn add_str(&mut self, text: &str);
    pub fn set_direction(&mut self, dir: hb_direction_t);
    pub fn set_script(&mut self, script: hb_script_t);
    pub fn set_language(&mut self, lang: &str);
    pub fn glyph_infos(&self) -> &[hb_glyph_info_t];
    pub fn glyph_positions(&self) -> &[hb_glyph_position_t];
}

impl Drop for HbBuffer {
    // hb_buffer_destroy
}

pub(crate) fn shape(font: &HbFont, buffer: &mut HbBuffer, features: &[hb_feature_t]);
```

The mapping from HarfBuzz C API to what we need:

| Our operation | HarfBuzz C API |
|---------------|----------------|
| Load font | `hb_blob_create` → `hb_face_create` → `hb_font_create` |
| Set direction | `hb_buffer_set_direction(HB_DIRECTION_LTR/RTL/TTB)` |
| Set script | `hb_script_from_string` → `hb_buffer_set_script` |
| Set language | `hb_language_from_string` → `hb_buffer_set_language` |
| Add text | `hb_buffer_add_utf8` |
| Parse features | `hb_feature_from_string` |
| Shape | `hb_shape(font, buffer, features, num_features)` |
| Read glyphs | `hb_buffer_get_glyph_infos` → `.codepoint` (gid), `.cluster` |
| Read positions | `hb_buffer_get_glyph_positions` → `.x_advance`, `.y_advance`, `.x_offset`, `.y_offset` |
| Variations | `hb_font_set_variations` |

This is a 1:1 mapping to what `RustyBuzzShaper` already does — the types are nearly identical because
rustybuzz was ported from HarfBuzz.

### Step 2: `HarfBuzzShaper` implementation

Create `sile-core/src/shaper_harfbuzz.rs` (gated behind `#[cfg(feature = "harfbuzz-native")]`).

```rust
pub struct HarfBuzzShaper;

impl Shaper for HarfBuzzShaper {
    fn shape(&self, text: &str, face: &FontFace, spec: &FontSpec) -> Vec<GlyphItem> {
        // 1. Create HbFont from face.raw_data()
        // 2. Create HbBuffer, set direction/script/language
        // 3. Parse features
        // 4. Call shape()
        // 5. Convert glyph_infos + glyph_positions → Vec<GlyphItem>
        //    (identical logic to RustyBuzzShaper lines 159-183)
    }
}
```

The conversion from HarfBuzz output to `GlyphItem` is identical to the rustybuzz path — same fields,
same scaling logic, same `extract_glyph_texts()` call, same `glyph_bounding_box()` query from
`FontFace` (which uses ttf-parser, independent of the shaper).

### Step 3: Font caching for HarfBuzz

`RustyBuzzShaper` currently creates a `rustybuzz::Face` on every `shape()` call. For HarfBuzz, the
`hb_font_t` is heavier to create (it also builds internal caches). We should cache it.

Options:
- **Per-call (match rustybuzz):** Simple, correct. HarfBuzz font creation is fast (~microseconds).
  Start here.
- **Thread-local cache:** `HashMap<(ptr, index), HbFont>` keyed on font data pointer + index.
  Add later if profiling shows font creation is a bottleneck.

Start with per-call to keep parity with rustybuzz and avoid lifetime complexity.

### Step 4: Wire up feature flags and shaper selection

In `sile-core/Cargo.toml`:
```toml
[features]
default = []
harfbuzz-native = ["dep:harfbuzz-sys"]
graphite = ["harfbuzz-native"]

[dependencies]
harfbuzz-sys = { version = "0.6", features = ["bundled"], optional = true }
```

In `sile-core/src/lib.rs` or `shaper.rs`:
```rust
#[cfg(feature = "harfbuzz-native")]
mod harfbuzz_ffi;
#[cfg(feature = "harfbuzz-native")]
mod shaper_harfbuzz;
```

In `sile-cli/Cargo.toml`:
```toml
[features]
default = ["sile-core/harfbuzz-native"]  # native CLI defaults to harfbuzz
graphite = ["sile-core/graphite"]
```

### Step 5: Graphite2 integration

This is the most build-system-heavy step. Options:

**Option A: Fork harfbuzz-sys**
- Add a `graphite` feature to the forked `harfbuzz-sys`
- In `build.rs`, when `graphite` is enabled:
  - Build graphite2 from vendored sources (via `cc` crate)
  - Pass `-DHB_HAVE_GRAPHITE2=ON` to the HarfBuzz build
  - Link both libraries

**Option B: Own `-sys` crate**
- Create `sile-harfbuzz-sys` that vendors both HarfBuzz + Graphite2
- More control, but more maintenance

**Option C: System library**
- `pkg-config` for `harfbuzz` with graphite support
- Simplest but least portable; user must install correct packages

Recommend **Option A** initially. The fork delta from servo's `harfbuzz-sys` would be small —
just the graphite2 vendoring in `build.rs`. If upstream later adds a graphite feature, we can
drop the fork.

### Step 6: Variations support

While we're adding HarfBuzz, wire up `FontSpec.variations` for both shapers:

```rust
// Parse "wght=700,wdth=50" into variation tuples
fn parse_variations(s: &str) -> Vec<(Tag, f32)> { … }
```

- **HarfBuzz:** `hb_font_set_variations(font, variations, count)`
- **rustybuzz:** `face.set_variations(variations)` (rustybuzz supports this)

### Step 7: Tests

The existing test suite in `shaper.rs` (lines 282-506) shapes text and checks glyph properties.
These tests should pass identically with both backends:

```rust
#[cfg(test)]
mod tests {
    // Existing tests run against RustyBuzzShaper

    #[cfg(feature = "harfbuzz-native")]
    mod harfbuzz_tests {
        // Same test body, using HarfBuzzShaper instead
        // Results should be identical (or very close for the 31 edge cases)
    }
}
```

Add a helper that runs tests against both shapers when `harfbuzz-native` is enabled, to catch
any divergence.

---

## File Changes Summary

| File | Change |
|------|--------|
| `sile-core/Cargo.toml` | Add `harfbuzz-sys` optional dep; add feature flags |
| `sile-core/src/harfbuzz_ffi.rs` | **New.** Safe wrappers around `harfbuzz-sys` FFI (~200 lines) |
| `sile-core/src/shaper_harfbuzz.rs` | **New.** `HarfBuzzShaper` implementing `Shaper` trait (~150 lines) |
| `sile-core/src/shaper.rs` | Add `default_shaper()` function; make `extract_glyph_texts()` `pub(crate)` |
| `sile-core/src/lib.rs` | Conditionally include new modules |
| `sile-cli/Cargo.toml` | Forward feature flags from `sile-core` |
| `RUST_PORT_PLAN.md` | Update shaper backend section to reflect this plan |

---

## What Doesn't Change

- `GlyphItem`, `CharMetrics`, `SpaceSettings` — unchanged
- `Shaper` trait — unchanged
- `FontFace` / `FontSpec` — unchanged (already has `raw_data()`, `variations`)
- `shape_with_fallbacks()` / `apply_tracking()` — unchanged (work with any `dyn Shaper`)
- All downstream consumers of shaped glyphs — unchanged
- WASM build — unchanged (never enables `harfbuzz-native`)

---

## Build Matrix

| Target | Features | Shaper |
|--------|----------|--------|
| `wasm32-unknown-unknown` | (none) | RustyBuzzShaper |
| Native (default CLI) | `harfbuzz-native` | HarfBuzzShaper |
| Native + Graphite | `harfbuzz-native`, `graphite` | HarfBuzzShaper + Graphite2 |
| Native (pure Rust) | (none) | RustyBuzzShaper |

---

## Risks and Mitigations

| Risk | Mitigation |
|------|------------|
| `harfbuzz-sys` bundled build is fragile on some platforms | CI matrix covering Linux, macOS, Windows; fall back to system lib |
| Graphite2 vendoring adds build complexity | Isolate in fork; document required system deps as fallback |
| HarfBuzz output differs subtly from rustybuzz | Cross-shaper test suite catches divergence; document known differences |
| `hb_font_t` per-call is slow | Profile first; add caching only if measured |
| Fork of harfbuzz-sys drifts from upstream | Pin to specific HarfBuzz version; periodic rebase |
