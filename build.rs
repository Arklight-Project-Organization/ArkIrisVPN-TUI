fn main() {
    println!("cargo:rerun-if-changed=ico/ArkIrisUI-Icon.ico");

    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();

        res.set_icon("ico/ArkIrisUI-Icon.ico");
        res.set("FileDescription", "ArkIris TUI Controller");
        res.set("ProductName", "ArkIrisUI");
        res.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        res.set("FileVersion", env!("CARGO_PKG_VERSION"));
        res.set("InternalName", "arkirisui.exe");
        res.set("OriginalFilename", "arkirisui.exe");
        res.set("CompanyName", "ArkIris Project");
        res.set("LegalCopyright", "Copyright (C) 2026 ArkIris Project");

        res.compile().expect("failed to compile Windows resources");
    }
}
