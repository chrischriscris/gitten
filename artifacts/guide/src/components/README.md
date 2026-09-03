// String-template components: small `(data) => string` functions for markup
// that repeats. Single-use markup stays as `?raw` partials in ../views/.
//
// Rule of thumb: repeated 3+ times (commit rows, file items, swatch cards)
// -> a function here. Appears once (whole TUI window) -> a partial.
//
// When modal redesign proposals land, they start here as `modal-*`
// components, not as edits to the big mockup partial. If this folder ever
// wants nested state or async, that is the signal to adopt a framework
// (the split already matches what React components would look like).
//
// NOTE: nothing here is rendered yet — the mockup partial still carries the
// static rows. First extraction happens with the modal work.

export {};
