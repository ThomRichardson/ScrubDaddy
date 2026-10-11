fn main() {
    println!("cargo:rerun-if-changed=resources.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/app.ico");
    embed_resource::compile("resources.rc", embed_resource::NONE).manifest_optional().unwrap();
}
