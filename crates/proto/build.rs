fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc");
    // SAFETY: build scripts are single-threaded at this point.
    unsafe { std::env::set_var("PROTOC", protoc) };
    println!("cargo:rerun-if-changed=proto/deskbridge.proto");
    prost_build::compile_protos(&["proto/deskbridge.proto"], &["proto"]).expect("compile protos");
}
