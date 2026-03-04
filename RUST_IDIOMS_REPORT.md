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

---

## 2. Idiomatic Rust Patterns

### 2.1 Implement `std::iter::Sum` for `Length`

The codebase repeats this fold pattern 6 times:

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

### 4.2 `max_node_dim` silently treats relative units as zero

```rust
let acc_pt = acc.to_pt().unwrap_or(0.0);
let l_pt = l.to_pt().unwrap_or(0.0);
```

If any node has a relative-unit dimension, it's silently treated as 0 in the max
comparison. This will produce incorrect layouts. At minimum, this should `debug_assert!`
or log a warning. Ideally, dimension comparison should require resolved (absolute) values.

### 4.3 `FontSpec` string fields should be typed

`features`, `variations`, `language`, `script` are bare `String` fields. These have
well-defined formats:

- **features**: OpenType feature tags (e.g., `"smcp"`, `"liga"`) — should be a
  `Vec<OTFeature>` where `OTFeature` is a validated 4-byte tag + value.
- **language**: BCP 47 / OpenType language tag — could be an enum or validated newtype.
- **script**: ISO 15924 / OpenType script tag — same treatment.

This prevents invalid data from propagating silently through the system.

---

## 5. Minor Polish

### 5.1 Missing `Default` derive for `Dim`

`Dim` (the dimension selector enum) doesn't derive `Default`, `Clone`, or `Copy`. It
should at minimum derive `Clone, Copy` since it's a fieldless enum used by value.

### 5.2 Inconsistent Display implementations

`Glue`, `Kern`, `VGlue`, `VKern` don't implement `Display` — their formatting is handled
in `Node::Display`. This means you can't format them independently. For consistency, each
struct should have its own `Display` impl, and `Node::Display` should delegate.

### 5.3 `Discretionary::clone_as_postbreak` uses assert

```rust
assert!(self.used, "Cannot clone a non-used discretionary as postbreak");
```

This should return `Result` or `Option` — the caller may want to handle this gracefully
rather than panicking.

### 5.4 Consider `#[non_exhaustive]` on public enums

If `Node`, `Unit`, `Color` etc. are intended to be extensible in future phases (new node
types, new units), marking them `#[non_exhaustive]` prevents downstream code from writing
exhaustive matches that would break on additions.

### 5.5 `format_f64` allocates a String for Display

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

---

## Summary

| Category | Item | Impact | Effort |
|---|---|---|---|
| Structural | 1.1 Extract `Dimensions` struct | High | Medium |
| Structural | 1.2 Collapse glue/vglue variants | High | Medium |
| Structural | 1.3 Cache parsed `FontFace` | Medium | Medium |
| Structural | 1.4 Typed `ColorError` | Low | Low |
| Idiom | 2.1 Implement `Sum` for `Length` | Low | Low |
| Idiom | 2.2 Compound assignment ops | Low | Low |
| Idiom | 2.3 `Cow<str>` for `to_text()` | Low | Low |
| Idiom | 2.4 Derive `node_type` or remove | Low | Low |
| Idiom | 2.5 Remove `is_*` methods | Low | Low |
| Idiom | 2.6 Reuse `sum_widths` | Low | Low |
| Perf | 3.1 `phf` for named colors | Medium | Low |
| Perf | 3.2 Hash-based font cache key | Low | Medium |
| Perf | 3.3 Use `str::find` | Low | Low |
| Perf | 3.4 Avoid Vec alloc in `append` | Low | Low |
| Safety | 4.1 Absolute/Relative measurement split | High | High |
| Safety | 4.2 Audit `unwrap_or(0.0)` for relative units | Medium | Low |
| Safety | 4.3 Typed FontSpec fields | Medium | Medium |
| Polish | 5.1–5.5 Various | Low | Low |

**Recommended priority order:** 1.1 → 1.2 → 2.1 → 2.2 → 3.1 → 3.3 → 5.5 → 1.3 → 4.1 (as a
larger follow-up). The first two items alone will eliminate ~60% of the boilerplate match arms in
`node.rs`.
