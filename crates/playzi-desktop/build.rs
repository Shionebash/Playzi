use std::fs;
use std::path::Path;

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("failed to create dist directory");
    let Ok(entries) = fs::read_dir(src) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target);
        } else if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
            if entry.file_name() == "index.html" {
                let html = fs::read_to_string(&path).expect("failed to read static/index.html");
                let rewritten = html.replace("/static/", "./");
                fs::write(&target, rewritten).expect("failed to write dist/index.html");
            } else {
                fs::copy(&path, &target).expect("failed to copy static asset to dist");
            }
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=../../static");
    let static_dir = Path::new("../../static");
    let dist_dir = Path::new("dist");
    if static_dir.exists() {
        copy_dir_recursive(static_dir, dist_dir);
    } else {
        fs::create_dir_all(dist_dir).expect("failed to create fallback dist dir");
    }
    tauri_build::build();
}
