//! Packs the WooCommerce plugin and the PrestaShop module into zips that the gateway serves
//! from its dashboard.

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

fn add(zip: &mut zip::ZipWriter<fs::File>, dir: &Path, prefix: &str) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        let name = format!("{prefix}/{}", path.file_name().unwrap().to_string_lossy());
        if path.is_dir() {
            add(zip, &path, &name);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .last_modified_time(zip::DateTime::default())
                .unix_permissions(0o644);
            zip.start_file(name, options).unwrap();
            zip.write_all(&fs::read(&path).unwrap()).unwrap();
        }
    }
}

fn pack(source: &str, file: &str) {
    let source = Path::new(source);
    println!("cargo:rerun-if-changed={}", source.display());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join(file);
    let mut zip = zip::ZipWriter::new(fs::File::create(out).unwrap());
    add(&mut zip, source, "xmrgate");
    zip.finish().unwrap();
}

fn main() {
    pack("woocommerce/xmrgate", "xmrgate-woocommerce.zip");
    pack("prestashop/xmrgate", "xmrgate-prestashop.zip");
}
