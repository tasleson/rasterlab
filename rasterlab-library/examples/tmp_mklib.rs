use rasterlab_library::Library;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).unwrap());
    let n: usize = std::env::args().nth(2).unwrap().parse().unwrap();
    let lib = Library::open_or_create(&root).unwrap();
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("test_images");
    let tmp = root.join("staging_src");
    std::fs::create_dir_all(&tmp).unwrap();
    let mut paths = Vec::new();
    for i in 0..n {
        let p = tmp.join(format!("img{i}.jpg"));
        let mut bytes = std::fs::read(src.join("meta_test.jpg")).unwrap();
        bytes.extend_from_slice(format!("{i}").as_bytes());
        std::fs::write(&p, &bytes).unwrap();
        paths.push(p);
    }
    let s = lib.import_files(&paths, |_| {}).unwrap();
    println!("imported, errors: {:?}", s.errors.len());
}
