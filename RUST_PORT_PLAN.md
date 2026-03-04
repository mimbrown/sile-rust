# Rust Port Feasibility Plan

## Executive Summary

A Rust port of SILE is **highly feasible** and well-motivated. The existing codebase already uses Rust
for its CLI layer (via `mlua`), which means the build infrastructure, dependency chain, and team are
already Rust-aware. The core algorithmic work (Knuth-Plass line breaking, Liang hyphenation, Cassowary
frame constraints, HarfBuzz text shaping) all have mature pure-Rust equivalents, making a WASM build
achievable without C dependency rewrites.

The primary pain point — Lua's dynamic typing — maps naturally onto Rust's type system. The existing
Lua code is already well-structured into swappable subsystems (inputters, shapers, typesetters,
outputters, pagebuilders), making it a good candidate for a typed Rust trait system.

---

## Current Architecture

```
┌─────────────────────────────────────────────────────────┐
│  CLI (Rust, clap)                                        │
│  src/cli.rs + src/lib.rs                                 │
└────────────────────┬────────────────────────────────────┘
                     │ mlua FFI
┌────────────────────▼────────────────────────────────────┐
│  Lua VM (mlua 0.10)                                      │
│  core/sile.lua → all business logic                      │
│  classes/, packages/, typesetters/, shapers/, etc.       │
└────┬──────────────┬───────────────┬──────────────────────┘
     │ FFI           │ FFI            │ FFI
┌────▼───┐    ┌─────▼──────┐  ┌─────▼──────────────┐
│HarfBuzz│    │libtexpdf   │  │ICU / fontconfig    │
│(C)     │    │(C, ~144    │  │(C, system libs)    │
│        │    │ source     │  │                    │
└────────┘    │ files)     │  └────────────────────┘
              └────────────┘
```

**What the Rust layer does today:** Almost nothing — it starts a Lua VM, injects search paths and
version info, and translates CLI args into Lua table values. Every algorithmic decision happens in Lua.

---

## Proposed Target Architecture

The core design principle: **the imperative builder API is the product**. Parsers, document classes,
and scripting layers are clients of that API — not the other way around. This means the API can be
used directly from Rust, from JS/TS via WASM, or driven by any parser without any one input format
being baked into the foundation.

```
┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐
│  SIL Parser      │  │  JS/TS direct    │  │  Future parsers  │
│  (post-v1)       │  │  API use         │  │  (Markdown, etc) │
└────────┬─────────┘  └────────┬─────────┘  └────────┬─────────┘
         │                     │                      │
         └─────────────────────┼──────────────────────┘
                               │ imperative calls
┌──────────────────────────────▼──────────────────────────────┐
│  Builder API  (sile-core)                                    │
│                                                              │
│  builder.set_font(...)      builder.add_text(...)           │
│  builder.push_frame(...)    builder.new_page()              │
│  builder.add_image(...)     builder.set_color(...)          │
│                                                              │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐   │
│  │  Shaper  │  │Typesetter│  │  Page    │  │Outputter │   │
│  │ (trait)  │  │ (trait)  │  │ Builder  │  │ (trait)  │   │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘   │
│       │              │             │              │          │
│  ┌────▼──────────────▼─────────────▼──────────────▼──────┐  │
│  │              Typed Node Graph (hbox/vbox/glue/…)      │  │
│  └───────────────────────────────────────────────────────┘  │
└────┬──────────────┬───────────────┬──────────────────────────┘
     │               │               │
┌────▼────┐  ┌───────▼──────┐ ┌─────▼──────┐
│rustybuzz│  │  ttf-parser  │ │pdf-writer  │
│(pure Rust│  │  + fontdb    │ │(pure Rust) │
│HarfBuzz) │  │  (pure Rust) │ │            │
└──────────┘  └──────────────┘ └────────────┘
```

All leaf dependencies are pure Rust → compiles to `wasm32-unknown-unknown` without modification.
The WASM and Node.js builds expose the same builder API to JS/TS consumers directly.

---

## Key Rust Crates Available

| Subsystem | Current (C/Lua) | Rust Replacement | WASM? |
|---|---|---|---|
| Text shaping (v1) | HarfBuzz (C) | `rustybuzz` 0.20 | ✅ pure Rust |
| Text shaping (post-v1) | HarfBuzz (C) | `harfbuzz-sys` + Graphite2 | ❌ native only |
| Font parsing | C in libtexpdf | `ttf-parser` 0.21 | ✅ pure Rust |
| Font database | fontconfig (C) | `fontdb` 0.16 | ✅ (bundled fonts) |
| Hyphenation | Liang in Lua | `hyphenation` 0.8 | ✅ pure Rust |
| PDF output | libtexpdf (C) | `pdf-writer` 0.12 | ✅ pure Rust |
| SVG output | N/A | `svg` / `resvg` | ✅ pure Rust |
| Line breaking | Lua (core/break.lua) | Custom Knuth-Plass | ✅ |
| Frame constraints | Cassowary in Lua | `cassowary` 0.3 | ✅ pure Rust |
| Bidi text | ICU (C) | `unicode-bidi` 0.3 | ✅ pure Rust |
| Unicode line break | ICU (C) | `unicode-linebreak` 0.2 | ✅ pure Rust |
| JS scripting | N/A | `rquickjs` or `boa` | ✅ both pure Rust |
| Node.js addon | N/A | `napi-rs` | Native only |
| WASM bindings | N/A | `wasm-bindgen` | ✅ |

---

## Feasibility Assessment by Subsystem

### ✅ Straightforward Ports

**Type system** — `types/node.lua`, `types/measurement.lua`, `types/length.lua`, `types/color.lua`.
These are pure data with arithmetic. Perfect Rust structs with `Display`, `Add`, `Mul` impls. The
measurement unit conversion table maps cleanly to an enum + conversion factor approach.

**Input parsers** — The SIL format is SILE's custom markup. A `nom` or `pest` parser in Rust would
be cleaner and safer than the current Lua parser. XML via `quick-xml`. Both are well-trodden Rust
territory.

**Hyphenation** — The Liang algorithm (`core/hyphenator-liang.lua`, 224 lines) and its 88+ language
pattern files can be replaced wholesale by the `hyphenation` crate which already ships the same TeX
hyphenation patterns.

**Settings system** — `core/settings.lua` is a key-value store with type validation. Straightforward
as a typed `HashMap` with a settings descriptor registry.

**Color** — `types/color.lua` maps trivially to a Rust enum `Color { Rgb(f32,f32,f32), Cmyk(...), Named(...) }`.

### ✅ Feasible but Substantial

**Line breaker** — `core/break.lua` (839 lines) implements Knuth-Plass. This is the most algorithmically
complex single file. It's well-understood (derived from TeX), and several Rust implementations exist
as reference (e.g., the `xi-editor` team wrote a Rust K-P implementation). Budget 2-3 weeks to get
it right with tests.

**Text shaping** — Use `rustybuzz` (v0.20, tracking HarfBuzz v10.1.0) for v1. It is more mature than
its version number suggests: it passes 2,221/2,252 HarfBuzz shaping tests (98.6%), tracks HarfBuzz
algorithm releases closely, and is used in production by cosmic-text (which powers the Zed editor).
The API maps closely to HarfBuzz, so this is mainly mechanical translation of the `shapers/harfbuzz.lua`
logic into typed Rust structs. The font fallback chain logic in `core/font.lua` will need careful handling.

The `Shaper` trait abstraction (see below) is important here: it isolates the rest of the pipeline
from which shaping backend is in use, making the post-v1 Graphite addition a purely additive change.

**PDF output** — `outputters/libtexpdf.lua` (13.8KB) calls into libtexpdf's C API. Replacing it with
`pdf-writer` requires understanding the PDF content stream model but is very doable. `pdf-writer` is
lower-level than libtexpdf so you'll need to handle font subsetting yourself — `subsetter` crate can
help, or embed `ttf-parser`'s subsetting support.

**Frame system** — `core/frame.lua` uses a Cassowary constraint solver for flexible page layout.
The `cassowary` crate is a direct port of the same algorithm. The frame direction system (LTR-TTB,
RTL-TTB, etc.) adds complexity but is bounded.

**Page builder** — `pagebuilders/base.lua` is heavily optimized Lua (~1/3 of runtime). A Rust port
will be faster but requires careful algorithmic fidelity for the TeX-style penalty/badness system.

### ⚠️ Complex, Plan Carefully

**Document class system** — Classes in Lua use prototype-based OOP (`pl.class`). In Rust, the natural
model is a `DocumentClass` trait with a set of required methods and a `BaseClass` struct that can
be composed. The challenge is that packages and classes share state through the global `SILE` table
— this needs a clean ownership model (likely `Arc<Mutex<DocumentState>>`).

**Package system** — 67 built-in packages. You won't port all of them in v1. Design the plugin API
first (a `Package` trait), implement core packages (color, lists, footnotes), and expose a JS/TS
scripting layer for user packages that don't need Rust-level performance.

**Math typesetting** — `packages/math/` is a large, complex subsystem. Consider delegating to
MathML-to-layout conversion using an existing library, or treat this as a post-v1 concern.

**Bidirectional text** — `unicode-bidi` handles the Unicode BiDi algorithm. The complexity is in
correctly interleaving LTR and RTL runs through the shaping and layout pipeline. Budget dedicated
time and test against the existing SILE bidi test cases.

### ❗ Requires Design Decisions

**JS/TS scripting interop** — Two options:
1. **`rquickjs`** (QuickJS bindings) — embeds a pure-Rust JS engine, compiles to WASM. Good for
   in-document scripting and user-defined packages in JS/TS. Lower overhead.
2. **`boa_engine`** — pure Rust JS engine, better spec compliance, also WASM-compatible.
3. **`deno_core`** (V8) — best JS performance and ecosystem, but brings V8's C++ dependency, no WASM.

**Recommendation:** Use `rquickjs` for the embeddable scripting layer (works in both native and WASM),
and expose a separate `napi-rs` binding for Node.js consumers who want to call sile from JS natively.

---

## Phased Implementation Plan

The phases are ordered to get a real PDF out of an imperative Rust API as fast as possible.
The parser is deliberately deferred — it is built on top of the finished API, not before it.

### Phase 0: Project Skeleton (Week 1-2)
- New Cargo workspace with crates: `sile-core`, `sile-cli`, `sile-wasm`, `sile-node`
- CI: `cargo test`, `cargo clippy`, `cargo build --target wasm32-unknown-unknown` all green from day one
- Define trait contracts for `Shaper`, `Outputter`, `PageBuilder` before implementing any of them
- No application logic yet — just scaffolding that proves the WASM target builds

**Deliverable:** Empty workspace that compiles to native and WASM.

### Phase 1: Core Types (Week 2-4)
- `Measurement` — full unit system (pt, mm, cm, in, em, ex, %fw, etc.) with arithmetic ops
- `Length` — stretchable space (natural + stretch + shrink), for glue nodes
- `Color` — RGB, CMYK, named colors
- Node graph: `HBox`, `VBox`, `Glue`, `Penalty`, `Kern`, `DiscretionaryBreak`
- All types `no_std`-compatible where possible; full test coverage

**Deliverable:** All data types compile to WASM; `cargo test` passes. This is the foundation
every later phase depends on — getting the types right here saves pain later.

### Phase 2: Font System (Week 4-7)
- `ttf-parser` for OpenType/TrueType parsing (metrics, glyph outlines, GSUB/GPOS tables)
- `fontdb` for font discovery on native platforms
- WASM font loading: fonts passed in as `&[u8]` — no filesystem access required
- Font metrics caching; OpenType feature negotiation

**Deliverable:** Can load a font file, query advance widths, map Unicode codepoints to glyph IDs.

### Phase 3: Text Shaping (Week 7-10)
- `rustybuzz` integration behind the `Shaper` trait
- Glyph run output: `(glyph_id, x_advance, y_advance, x_offset, y_offset)`
- Font fallback chain for missing glyphs
- RTL and bidi run handling

**Deliverable:** Can shape a paragraph of Latin, Arabic, and mixed-direction text into
positioned glyph runs. Compiles and runs identically in native and WASM.

### Phase 4: Hyphenation and Line Breaking (Week 10-15)
- `hyphenation` crate with language pattern loading (same TeX patterns SILE uses)
- Knuth-Plass line breaking (port from `core/break.lua`, 839 lines)
  - Demerits and badness calculation
  - Fitness classes, active node list
  - Hyphenation penalties
  - Widow/orphan control via penalty injection
- Paragraph pipeline: `&str` → shaped runs → broken lines → `[HBox]`

**Deliverable:** Can break a paragraph into optimally-broken lines. Validated against
the existing `spec/break_spec.lua` test cases.

### Phase 5: Frame and Page Layout (Week 15-19)
- `cassowary` constraint solver for frame geometry
- Frame definition: position, size, writing direction, next-frame pointer
- Multi-frame page layouts (main body, header, footer, margin notes)
- Page builder: accumulate `VBox` content, find optimal page breaks
- Widow/orphan handling at page level

**Deliverable:** Can distribute paragraphs across multiple pages with a header/footer frame.

### Phase 6: PDF Output (Week 19-23)
- `pdf-writer` for content streams and document structure
- TrueType font subsetting and embedding (via `ttf-parser` subsetting API)
- Glyph positioning, color, rotation
- Image embedding (JPEG, PNG via `image` crate)
- Hyperlink annotations, document outline (bookmarks)

**Deliverable:** Can render a multi-page, multi-font document to a valid PDF. Visually
validate against existing SILE regression test outputs.

### Phase 7: Imperative Builder API and JS/TS Bindings (Week 23-29)
This is the phase that defines the product's public surface. Everything built so far has been
internal infrastructure; now it gets a clean, stable API.

**Builder API (Rust):**
```rust
let mut doc = DocumentBuilder::new(PaperSize::A4);
doc.load_font("body", FontSpec { path: "...", size: pt(11.0), .. })?;
doc.set_font("body");
doc.add_text("Hello, world.");
doc.new_paragraph();
doc.push_frame(FrameSpec { .. });
let pdf: Vec<u8> = doc.render()?;
```

**WASM build (`sile-wasm`, `wasm-bindgen`):**
- Export the builder API to JS/TS via `wasm-bindgen` and `tsify`
- Fonts passed as `Uint8Array`; output returned as `Uint8Array`
- Streaming page callbacks for progressive rendering
- npm package with TypeScript type definitions

**Node.js native addon (`sile-node`, `napi-rs`):**
- Same API surface as WASM but with filesystem font loading

**Deliverable:** A JS/TS user can build a PDF programmatically without a parser:
```ts
import init, { DocumentBuilder } from '@sile/wasm';
await init();
const doc = new DocumentBuilder({ paper: 'A4' });
doc.loadFont('body', fontBytes);
doc.addText('Hello, world.');
const pdf = doc.render();
```

### Phase 8: Document Class System (Week 29-33)
Now that the builder API is solid, layer document class conventions on top of it.

- `DocumentClass` trait with lifecycle hooks (`new_page`, `finish_page`, `finish`)
- `BaseClass` providing sensible defaults (margins, body font, running headers)
- `PlainClass` — single content frame
- `BookClass` — recto/verso, chapter headings, running headers
- These are Rust structs that call the builder API, not a separate abstraction layer

**Deliverable:** Can produce a well-formatted book PDF by calling into document class methods.

### Phase 9: Built-in Content Features (Week 33+, ongoing)
With the API and class system stable, add content capabilities in priority order:

**Tier 1 (core document needs):**
- Footnotes — collect during page build, place in footer frame
- Running headers/footers — populated from document class hooks
- Rules, color, image embedding (already partly done in Phase 6)
- Table of contents — two-pass with page number back-fill

**Tier 2 (common documents):**
- Lists (ordered, unordered, definition)
- Hyperlinks (internal cross-references + external URLs)
- Verbatim / code blocks

**Tier 3 (advanced):**
- Bidirectional text (RTL documents, mixed LTR/RTL)
- Multi-column parallel text
- Math typesetting (post-v1; substantial scope)
- SVG embedding via `resvg`

### Phase 10: SIL Parser (post-v1)
Only after the builder API is stable and well-tested does it make sense to build the parser,
because the parser is just a client — it translates markup into builder API calls.

- SIL format parser using `nom` or `pest`
  - Commands: `\command[opt=val]{content}`
  - Environments: `\begin{env}...\end{env}`
- Parser emits a typed `Ast` which is then evaluated against the builder API
- XML inputter via `quick-xml` using the same evaluation path
- Round-trip testing: `parse(sil) → evaluate → render` matches existing SILE outputs
- Fuzz the parser independently of the layout engine

---

## Shaper Backend Strategy

### v1: rustybuzz only

`rustybuzz` covers the vast majority of scripts needed for general typesetting: Latin, CJK, Arabic
(standard OpenType), Hebrew, Devanagari, and all other scripts with OpenType shaping tables. It is
pure Rust, compiles to `wasm32-unknown-unknown` without any extra tooling, and integrates cleanly
with `wasm-bindgen`. The 1.5–2× performance gap relative to C HarfBuzz is irrelevant at typesetting
scale.

Known gaps in rustybuzz that do **not** affect v1 scope:
- No Graphite shaping (see post-v1 below)
- No AAT shaping (Apple Advanced Typography — macOS system fonts only, not user-loaded OpenType)
- 31 edge-case shaping tests failing out of 2,252

### Post-v1: C HarfBuzz + Graphite2 behind a feature flag

**Awami Nastaliq** (SIL's font for Urdu Nastaliq script) and other complex minority-script fonts
require **Graphite shaping** — a layout engine developed by SIL that handles positioning rules too
complex for OpenType to express. Graphite support in HarfBuzz requires building HarfBuzz with
`Graphite2` as a compile-time dependency (`-DHB_HAVE_GRAPHITE2=ON`).

The addition will look like:

```toml
# Cargo.toml
[features]
default = []
graphite = ["harfbuzz-sys"]   # pulls in vendored HarfBuzz + Graphite2; native only
```

```rust
// Shaper trait — defined in v1, unchanged post-v1
trait Shaper: Send + Sync {
    fn shape(&self, text: &str, font: &Font, features: &[Feature]) -> Vec<GlyphInfo>;
}

// v1 implementation
struct RustyBuzzShaper { ... }
impl Shaper for RustyBuzzShaper { ... }

// post-v1 addition
#[cfg(feature = "graphite")]
struct HarfBuzzGraphiteShaper { ... }
#[cfg(feature = "graphite")]
impl Shaper for HarfBuzzGraphiteShaper { ... }
```

The `graphite` feature is mutually exclusive with the WASM build — the WASM target always uses
`RustyBuzzShaper`. Documents that require Graphite shaping in a non-graphite build will receive a
clear error at font-load time rather than silently producing incorrect output.

The vendoring job for `harfbuzz-sys` with Graphite2 enabled is non-trivial (two C libraries, CMake
or Meson build, Graphite2 as a git submodule of HarfBuzz) but entirely self-contained. It does not
touch any other part of the codebase.

---

## Architecture Decisions to Nail Down First

### 1. Threading Model
Lua is single-threaded; Rust can be multi-threaded. Options:
- **Single-threaded** (simplest, matches current behavior) — use `Rc<RefCell<>>` internally
- **Send + Sync** (enables parallel page rendering) — use `Arc<Mutex<>>` for shared state

Recommendation: Start single-threaded (`!Send`), add parallelism later via rayon for page rendering.

### 2. Error Handling
Lua uses `pcall` for recoverable errors. In Rust, use `thiserror` for typed error enums per crate,
`anyhow` at the CLI boundary. Define a `SileError` hierarchy covering: ParseError, FontError,
LayoutError, OutputError.

### 3. Scripting API Surface
Define what JS/TS can do vs what must be Rust:
- **JS-accessible:** command registration, settings declarations, content callbacks, style computation
- **Rust-only:** shaping, line/page breaking, font loading, PDF binary output

This is the key design decision — the right boundary determines extensibility vs safety tradeoffs.

### 4. Parser as a Client, Not the Core
The SIL parser is explicitly post-v1 and is a consumer of the builder API rather than a
foundational layer. This means the API must be expressive enough that any reasonable input
format can drive it — but the API design should not be shaped by what the SIL format happens
to look like. Design the API for direct programmatic use first; adapt the parser to it later.

---

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Knuth-Plass fidelity gaps | Medium | High | Port test suite from `spec/break_spec.lua` first |
| Font rendering differences | Medium | Medium | Visual regression tests against existing expected outputs |
| rustybuzz shaping gaps (31 failing tests) | Low | Low | Review failing tests against target script list; gaps are edge cases in complex scripts |
| Awami Nastaliq / Graphite scripts | High | Medium | Deferred to post-v1; clear error in v1 builds rather than silent failure |
| Bidi complexity | High | Medium | Treat RTL as a separate milestone; use `unicode-bidi` + testing corpus |
| Package ecosystem gap | High | Medium | JS scripting layer reduces the need for Rust packages |
| Math typesetting scope | High | High | Defer to post-v1; use MathML → layout bridge |
| WASM bundle size | Low | Medium | `wasm-opt` + font subsetting; target <2MB for core |
| PDF spec compliance | Low | High | `pdf-writer` is well-tested; validate with `pdfinfo`/Acrobat |

---

## Suggested Next Steps

1. **Create the workspace** — `sile-core`, `sile-cli`, `sile-wasm` crates; CI green on both native
   and `wasm32-unknown-unknown` from commit one
2. **Define the trait contracts** — `Shaper`, `Outputter`, `PageBuilder` as empty traits with
   doc comments describing their contracts; nothing implemented yet
3. **Port the type system** — `Measurement`, `Length`, `Color`, node types; these are the
   foundation everything else builds on and the easiest wins
4. **Prototype the shaping pipeline** — `rustybuzz` + `ttf-parser` → glyph runs in WASM;
   this is the riskiest external dependency and worth proving out early
5. **Design the builder API surface on paper** — before implementing Phase 7, sketch the full
   public API as Rust method signatures and the equivalent TypeScript declarations; get the
   ergonomics right before anything depends on it

The builder API design (step 5) is the most important architectural investment. Everything else
is implementation — the API surface is what users and the eventual parser will be locked into.
