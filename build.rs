/// What `ui/art.slint` says when the art has not been fetched: there is none. The window then
/// falls back to letters and text. `tools\make-art.ps1 -Download` writes the real one.
const NO_ART: &str = "// No art fetched. Run tools\\make-art.ps1 -Download to get it; this file is then rewritten.\n\n\
export global Art {\n\
    pure public function has-face(agent: string) -> bool { false }\n\
    pure public function face(agent: string) -> image { @image-url(\"../assets/icon/sightline.png\") }\n\
    pure public function has-panel(agent: string) -> bool { false }\n\
    pure public function panel(agent: string) -> image { @image-url(\"../assets/icon/sightline.png\") }\n\
    pure public function has-rank(tier: int) -> bool { false }\n\
    pure public function rank(tier: int) -> image { @image-url(\"../assets/icon/sightline.png\") }\n\
    pure public function has-map(name: string) -> bool { false }\n\
    pure public function map(name: string) -> image { @image-url(\"../assets/icon/sightline.png\") }\n\
}\n";

fn main() {
    if !std::path::Path::new("ui/art.slint").exists() {
        std::fs::write("ui/art.slint", NO_ART).expect("could not write ui/art.slint");
    }

    // Pictures are put inside the exe as they are on disk (PNG and JPEG) and decoded when first
    // drawn, so the exe does not depend on the project folder being there.
    let config = slint_build::CompilerConfiguration::new().embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/app.slint", config).expect("slint compile failed");
    println!("cargo:rerun-if-changed=ui/art.slint");

    // The icon stored in the exe file itself. The window's icon is set in ui/app.slint.
    println!("cargo:rerun-if-changed=assets/icon/sightline.ico");
    println!("cargo:rerun-if-changed=assets/icon/sightline.png");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/icon/sightline.ico");
        // Task Manager shows the file description as the process name.
        resource.set("FileDescription", "Sightline");
        resource.set("ProductName", "Sightline");
        resource.compile().expect("icon resource");
    }
}
