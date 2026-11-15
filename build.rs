// Встраивает иконку в mysolid3d.exe (её видно в проводнике и на панели задач).
fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=иконка не встроена в exe: {e}");
        }
    }
}
