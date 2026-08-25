//! `cargo run -p resonator-ffi --features bindgen --bin uniffi-bindgen`:
//! generates the Swift/Kotlin bindings from the built library
//! (proc-macro metadata, `--library` mode; see mobile/build-*.sh).

fn main() {
    uniffi::uniffi_bindgen_main()
}
