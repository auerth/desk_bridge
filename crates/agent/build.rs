fn main() {
    // Optimus / PowerXpress: ask the driver to run this process on the high-performance GPU, so
    // capture and NVENC share the NVIDIA adapter on hybrid laptops (plan 8.7, ADR 0005).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg-bins=/EXPORT:NvOptimusEnablement,DATA");
        println!("cargo:rustc-link-arg-bins=/EXPORT:AmdPowerXpressRequestHighPerformance,DATA");
    }
}
