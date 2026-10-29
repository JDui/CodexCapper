fn main() {
    #[cfg(windows)]
    {
        let output = std::env::var("OUT_DIR").expect("OUT_DIR");
        println!("cargo:rerun-if-changed=native");
        println!("cargo:rerun-if-changed=../scripts/build-taskbar-bridge.ps1");
        let status = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                "../scripts/build-taskbar-bridge.ps1", "-OutDir", &output])
            .status().expect("could not start the native taskbar bridge compiler");
        assert!(status.success(), "native taskbar bridge compilation failed");
    }
    println!("cargo:rerun-if-changed=windows-app-manifest.xml");
    let windows = tauri_build::WindowsAttributes::new()
        .app_manifest(include_str!("windows-app-manifest.xml"));
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
        .expect("failed to build application resources");
}
