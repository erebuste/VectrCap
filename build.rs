// Embeds the VectrCap icon (assets/logo/vectrcap.ico) into the exe for Explorer and shortcuts.
fn main() {
    println!("cargo:rerun-if-changed=assets/logo/vectrcap.rc");
    println!("cargo:rerun-if-changed=assets/logo/vectrcap.ico");
    embed_resource::compile("assets/logo/vectrcap.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("compile the icon resource");
}
