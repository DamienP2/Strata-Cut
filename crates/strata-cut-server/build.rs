//! Makes sure web/dist exists (the server builds without the web app) and writes
//! gzip copies of its text files for `web.rs`; builds the DeckLink shim
//! (`decklink/shim.cpp`, Blackmagic's SDK headers in `decklink-sdk/linux`) on Linux.
use std::{env, fs, io::Write, path::{Path, PathBuf}};

fn main() {
    decklink();
    let dist = Path::new("../../web/dist");
    let _ = fs::create_dir_all(dist);
    println!("cargo:rerun-if-changed=../../web/dist");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("web-gz");
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).unwrap();
    let mut list = String::from("&[\n");
    walk(dist, dist, &out, &mut list);
    list.push_str("]\n");
    fs::write(PathBuf::from(env::var("OUT_DIR").unwrap()).join("web_gz.rs"), list).unwrap();
}

fn walk(root: &Path, dir: &Path, out: &Path, list: &mut String) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            println!("cargo:rerun-if-changed={}", p.display());
            walk(root, &p, out, list);
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !["js", "mjs", "css", "html", "svg", "json", "txt", "map", "ico", "xml"].contains(&ext) {
            continue;
        }
        let data = fs::read(&p).unwrap();
        if data.len() < 512 {
            continue;
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gz.write_all(&data).unwrap();
        let gz = gz.finish().unwrap();
        if gz.len() * 10 > data.len() * 9 {
            continue;
        }
        let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let target = out.join(rel.replace('/', "__"));
        fs::write(&target, gz).unwrap();
        list.push_str(&format!("    ({:?}, include_bytes!({:?}) as &[u8]),\n", rel, target.display().to_string()));
    }
}

/// The DeckLink shim: a C ABI over Blackmagic's SDK. It needs a C++ compiler only: the DeckLink
/// runtime (Desktop Video) is loaded at run time, so it is needed neither to build nor to start.
fn decklink() {
    println!("cargo::rustc-check-cfg=cfg(decklink_shim)");
    println!("cargo:rerun-if-changed=decklink");
    println!("cargo:rerun-if-changed=decklink-sdk");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") || env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("musl") {
        println!("cargo:warning=DeckLink capture left out of this build (Linux with glibc only)");
        return;
    }
    let mut b = cc::Build::new();
    b.cpp(true).file("decklink/shim.cpp").file("decklink-sdk/linux/DeckLinkAPIDispatch.cpp").include("decklink-sdk/linux").flag("-std=c++11").warnings(false);
    match b.try_compile("strata_decklink") {
        Ok(()) => {
            println!("cargo:rustc-link-lib=dl");
            println!("cargo:rustc-link-lib=pthread");
            println!("cargo:rustc-cfg=decklink_shim");
        }
        Err(e) => panic!("cannot build the DeckLink shim (a C++ compiler is needed: g++): {e}"),
    }
}
