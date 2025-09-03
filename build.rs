// build.rs
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        winres::WindowsResource::new()
            .set_icon("icon.ico") // Řekne kompilátoru, aby použil tento soubor jako ikonu
            .compile()
            .unwrap();
    }
}