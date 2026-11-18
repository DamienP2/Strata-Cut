//! Makes sure web/dist exists (the server builds without the web app) and writes
//! gzip copies of its text files for `web.rs`.
use std::{env, fs, io::Write, path::{Path, PathBuf}};

fn main() {
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
