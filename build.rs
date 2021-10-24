use flatc_rust;

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=flatbuf/introduction.fbs");
    flatc_rust::run(flatc_rust::Args {
        inputs: &[Path::new("flatbuf/introduction.fbs")],
        out_dir: Path::new("target/flatbuffers/"),
        ..Default::default()
    })
    .expect("flatc");
}
