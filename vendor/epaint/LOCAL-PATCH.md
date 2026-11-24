Upstream: https://github.com/emilk/egui/tree/0.36.1/crates/epaint
Version: epaint 0.36.1, MIT OR Apache-2.0 (licenses included).

Local changes: text/coretext.rs, text/mod.rs, text/font.rs.
For the application's macos-chinese / medium / semibold faces, if skrifa has no glyph outline,
macOS CoreText supplies the matching PingFangSC-Regular, Medium or Semibold outline. This handles macOS 26
PingFang's proprietary hvgl format. Standard font outlines, text layout, the
atlas, glyph cache, and renderer remain upstream implementations.
CoreText fonts are cached per physical size and weight per thread and released on thread
exit. CGPath and transient CFString references are released after use. The FFI
unsafe-code allowance is restricted to the CoreText module. Font glyph IDs must
come from the resolved PingFang SC face, and this adapter maps only weights 400/500/600 to their installed named instances.
Do not reuse it for arbitrary fonts or other variable weights.
No Apple font files are distributed. The app resolves installed fonts at runtime.
