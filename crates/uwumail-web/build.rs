//! Embeds the built web app (`web/dist`, or `UWUMAIL_WEB_DIST`) into the binary.
//!
//! Without a build the server still works and shows its simple landing page.
//! After `pnpm build` in `web/` for the first time, touch this file so Cargo
//! notices the new folder.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else if !path.file_name().is_some_and(|name| name.to_string_lossy().starts_with('.')) {
            files.push(path);
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=UWUMAIL_WEB_DIST");
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let dist =
        env::var_os("UWUMAIL_WEB_DIST").map(PathBuf::from).unwrap_or_else(|| manifest_dir.join("../../web/dist"));

    let mut files = Vec::new();
    if dist.join("index.html").is_file() {
        // Only watched when it exists: a missing path would rerun this script on every build.
        println!("cargo:rerun-if-changed={}", dist.display());
        collect(&dist, &mut files);
    }

    let mut entries: Vec<(String, PathBuf)> = files
        .into_iter()
        .map(|file| {
            let relative = file.strip_prefix(&dist).expect("inside dist").to_string_lossy().replace('\\', "/");
            (format!("/{relative}"), fs::canonicalize(&file).expect("readable asset"))
        })
        .collect();
    entries.sort();

    let mut code = String::from("pub static ASSETS: &[Asset] = &[\n");
    for (path, file) in &entries {
        writeln!(
            code,
            "    Asset {{ path: {path:?}, content_type: {:?}, bytes: include_bytes!({:?}) }},",
            content_type(file),
            file.display().to_string()
        )
        .expect("writing to a string");
    }
    code.push_str("];\n");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("assets.rs");
    fs::write(out, code).expect("writing assets.rs");
}
