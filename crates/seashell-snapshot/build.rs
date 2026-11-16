//! Compiles Firedancer's streaming snapshot parser (`fd_ssparse`) out of a Firedancer checkout,
//! together with the two small Firedancer sources it calls into and our C wrapper.
//!
//! The checkout is `../firedancer` next to this repository, or `FIREDANCER_DIR`.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let firedancer = std::env::var("FIREDANCER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest_dir.join("../../../firedancer"));
    let firedancer_src = firedancer.join("src");
    assert!(
        firedancer_src.join("discof/restore/utils/fd_ssparse.c").exists(),
        "no Firedancer checkout at {} (set FIREDANCER_DIR)",
        firedancer.display()
    );

    let firedancer_sources = [
        "discof/restore/utils/fd_ssparse.c",
        "ballet/base58/fd_base58.c",
        "flamenco/runtime/fd_system_ids.c",
    ];
    let own_sources = ["c/seashell_ssparse.c", "c/fd_log_shim.c"];

    let mut build = cc::Build::new();
    build
        .std("c17")
        .define("_GNU_SOURCE", None)
        .opt_level(3)
        .flag("-march=native")
        .warnings(false)
        .include(&firedancer_src);
    for source in firedancer_sources {
        let path = firedancer_src.join(source);
        println!("cargo:rerun-if-changed={}", path.display());
        build.file(path);
    }
    for source in own_sources {
        println!("cargo:rerun-if-changed={source}");
        build.file(manifest_dir.join(source));
    }
    println!("cargo:rerun-if-env-changed=FIREDANCER_DIR");
    build.compile("seashell_ssparse");
}
