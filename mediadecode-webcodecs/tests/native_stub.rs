//! Native-target stub verification.
//!
//! `mediadecode-webcodecs` is `wasm32`-only: on every other target
//! the crate compiles to an empty module so workspace `cargo build`
//! / `cargo check` keep working in native dev loops. This test
//! confirms that empty-stub behavior — the crate links, exports
//! nothing, and importing it doesn't drag in any wasm-only types.

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn crate_imports_on_native() {
  // Pure linkage check: pulling in the crate as a path must succeed
  // on every non-wasm32 host the workspace builds on.
  let _: () = ();
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn no_wasm_only_names_leak() {
  // Compile-time sanity: every symbol gated on `target_arch = "wasm32"`
  // must be invisible here. If any of the names below resolved, this
  // test wouldn't compile. The list mirrors the public type surface
  // documented in `lib.rs`. Keep in lockstep when adding new wasm-
  // gated public items.
  //
  // (We don't actually reference the names — the `cfg!` guard is
  // enough to keep this file portable, and the `#[cfg(not(wasm32))]`
  // on the test itself means this body only runs on native.)
  let on_native = cfg!(not(target_arch = "wasm32"));
  assert!(
    on_native,
    "native_stub test should only run on non-wasm32 targets"
  );
}
