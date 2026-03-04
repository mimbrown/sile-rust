# Bundled Graphite2 Support for HarfBuzz

## Current State

We link against the system HarfBuzz via `harfbuzz-sys` (without the `bundled` feature).
The Homebrew HarfBuzz is compiled with Graphite2 support, so Graphite shaping works
out of the box on macOS. On other platforms, it depends on the system package having
Graphite2 enabled.

## Goal

Vendor both HarfBuzz and Graphite2 so that `cargo build` produces a fully
self-contained binary with Graphite support, without requiring any system libraries.
This is important for cross-compilation and reproducible builds.

## Approach: Fork harfbuzz-sys

The `harfbuzz-sys` crate (v0.6, from the Servo project) already bundles the HarfBuzz
C++ source. Its `build.rs` compiles `harfbuzz/src/harfbuzz.cc` with `cc::Build`.
That file already `#include`s `hb-graphite2.cc`, which is guarded by
`#ifdef HAVE_GRAPHITE2`. The code is there — it just needs the define and the
Graphite2 library.

### Step 1: Create local fork

Copy `harfbuzz-sys` into the workspace:

```
crates/harfbuzz-sys/
  Cargo.toml
  build.rs
  src/
  harfbuzz/        # vendored HarfBuzz source (from upstream)
  graphite2/       # vendored Graphite2 source
```

### Step 2: Vendor Graphite2

Clone or copy Graphite2 source (MIT license, ~50 C/C++ files from `graphite2/src/`):
https://github.com/nicovank/graphite2

Only the `src/` directory and `include/graphite2/` headers are needed.

### Step 3: Add `graphite` feature to the forked harfbuzz-sys

In `crates/harfbuzz-sys/Cargo.toml`:

```toml
[features]
default = []
bundled = []
graphite = ["bundled"]  # graphite requires bundled build
coretext = []
directwrite = []
```

### Step 4: Update build.rs

In `crates/harfbuzz-sys/build.rs`, add Graphite2 compilation when the feature is
enabled:

```rust
fn build_harfbuzz() {
    // ... existing code ...

    if cfg!(feature = "graphite") {
        cfg.define("HAVE_GRAPHITE2", "1");
        cfg.include("graphite2/include");

        // Build graphite2 as a separate static library
        let mut gr2 = cc::Build::new();
        gr2.cpp(false)
            .warnings(false)
            .include("graphite2/include")
            .include("graphite2/src");

        // Add all graphite2 C source files
        for entry in std::fs::read_dir("graphite2/src").unwrap() {
            let path = entry.unwrap().path();
            if path.extension().map(|e| e == "cpp" || e == "c").unwrap_or(false) {
                // Skip files in subdirectories (like direct_machine)
                if path.parent().unwrap().file_name().unwrap() == "src" {
                    gr2.file(&path);
                }
            }
        }
        gr2.compile("embedded_graphite2");
    }

    cfg.compile("embedded_harfbuzz");
    // ...
}
```

### Step 5: Wire up in workspace

In the workspace `Cargo.toml`:

```toml
[patch.crates-io]
harfbuzz-sys = { path = "crates/harfbuzz-sys" }
```

In `sile-core/Cargo.toml`:

```toml
harfbuzz-sys = { version = "0.6", features = ["bundled", "graphite"] }
```

### Step 6: Feature flag plumbing

Add a `graphite` feature to sile-core and sile-cli that controls whether
Graphite is available:

```toml
# sile-core/Cargo.toml
[features]
default = ["graphite"]
wasm = []
graphite = ["harfbuzz-sys/graphite"]
```

When `wasm` is enabled, `harfbuzz-sys` is not used at all (rustybuzz takes over),
so `graphite` is irrelevant for WASM builds.

## Graphite2 source files needed

From the Graphite2 repo, the minimum set:

```
graphite2/
  include/graphite2/
    Font.h
    Segment.h
    Types.h
    Log.h
  src/
    CmapCache.cpp
    Code.cpp
    Collider.cpp
    Decompressor.cpp
    Face.cpp
    FeatureMap.cpp
    FileFace.cpp
    Font.cpp
    GlyphCache.cpp
    GlyphFace.cpp
    Intervals.cpp
    Justifier.cpp
    NameTable.cpp
    Pass.cpp
    Position.cpp
    Segment.cpp
    Silf.cpp
    Slot.cpp
    Sparse.cpp
    TtfUtil.cpp
    UtfCodec.cpp
    gr_char_info.cpp
    gr_face.cpp
    gr_features.cpp
    gr_font.cpp
    gr_logging.cpp
    gr_segment.cpp
    gr_slot.cpp
    json.cpp
    call_machine.cpp    # OR direct_machine.cpp (platform-dependent)
```

## Testing

After implementing, verify Graphite shaping works with a Graphite-enabled font
(e.g., Padauk for Myanmar, Scheherazade for Arabic, Charis SIL):

```rust
#[test]
fn harfbuzz_graphite_shaping() {
    // Load a Graphite-enabled font
    // Shape text with Graphite features
    // Verify output differs from OpenType-only shaping
}
```

## References

- HarfBuzz Graphite integration: `harfbuzz/src/hb-graphite2.cc`
- Graphite2 source: https://github.com/nicovank/graphite2
- SIL Graphite: https://graphite.sil.org/
- harfbuzz-sys crate: https://crates.io/crates/harfbuzz-sys
