//! Stages the built web client for embedding. If `client/dist` has not
//! been built (`npm run build` in `client/`), a placeholder page is staged
//! instead so the server still compiles and says what is missing.

use std::path::{Path, PathBuf};
use std::{env, fs, io};

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let dist = manifest.join("../../client/dist");
    let staged = PathBuf::from(env::var("OUT_DIR").expect("set by cargo")).join("client");
    println!("cargo:rerun-if-changed={}", dist.display());
    if staged.exists() {
        fs::remove_dir_all(&staged)?;
    }
    if dist.join("index.html").is_file() {
        copy_dir(&dist, &staged)?;
    } else {
        println!("cargo:warning=client/dist not found: embedding a placeholder page");
        fs::create_dir_all(&staged)?;
        fs::write(
            staged.join("index.html"),
            "<!doctype html><meta charset=\"utf-8\"><title>Resonator</title>\
             <p>The server is running, but it was built without the web client. \
             Run <code>npm run build</code> in <code>client/</code> and rebuild.</p>",
        )?;
    }
    Ok(())
}
