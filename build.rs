fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=src/date_history.c");
        cc::Build::new()
            .file("src/date_history.c")
            .compile("date_history");
        println!("cargo:rustc-link-lib=framework=CoreServices");
    }
}
