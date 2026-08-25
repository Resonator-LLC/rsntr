//! Embeds the Python interpreter's library directory as an rpath so the
//! `cargo test` binaries (which link libpython, unlike the maturin-built
//! extension module) can start without DYLD/LD_LIBRARY_PATH help.

fn main() {
    if std::env::var_os("CARGO_FEATURE_EXTENSION_MODULE").is_some() {
        return;
    }
    if let Some(lib_dir) = &pyo3_build_config::get().lib_dir {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_dir}");
    }
}
