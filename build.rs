fn main() {
    println!("cargo:rerun-if-changed=assets/teams-chat-exporter.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");

    #[cfg(target_os = "windows")]
    {
        let mut resources = winres::WindowsResource::new();
        resources.set_icon("assets/teams-chat-exporter.ico");
        let mut version = env!("CARGO_PKG_VERSION").split('.');
        if let (Some(year), Some(month), Some(day)) =
            (version.next(), version.next(), version.next())
        {
            let date_version = format!("{}{:0>2}{:0>2}", year, month, day);
            resources
                .set("FileVersion", &date_version)
                .set("ProductVersion", &date_version);
        }
        resources
            .compile()
            .expect("failed to embed the Windows icon and version information");
    }
}
