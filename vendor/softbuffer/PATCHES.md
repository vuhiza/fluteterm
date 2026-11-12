# Local patches to softbuffer 0.4.8

Vendored from crates.io (MIT / Apache-2.0, see `LICENSE-*`) and wired in through
`[patch.crates-io]` in the top-level `Cargo.toml`. All changes are in
`src/backends/cg.rs` (macOS / CoreGraphics backend):

1. **Alpha channel**: frames use `kCGImageAlphaPremultipliedFirst` instead of
   `NoneSkipFirst`, so transparent pixels stay transparent (needed for the
   see-through, blurred panel).
2. **Screen colour space**: frames are tagged with the main screen's colour space
   instead of generic device RGB, so Core Animation composites them without a
   per-frame colour conversion (about 3× less CPU).
3. **Buffer recycling**: frame buffers are returned to a small pool when Core
   Animation releases them, instead of allocating ~13 MB per frame.
