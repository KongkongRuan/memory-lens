use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=spd-bridge/MemoryLens.SpdBridge.csproj");
    println!("cargo:rerun-if-changed=spd-bridge/Program.cs");
    println!("cargo:rerun-if-changed=spd-bridge/LocalPawnIO.cs");
    println!("cargo:rerun-if-changed=spd-bridge/PawnIOService.cs");
    println!("cargo:rerun-if-changed=spd-bridge/packages.lock.json");
    println!("cargo:rerun-if-changed=spd-bridge/Resources");
    println!("cargo:rerun-if-env-changed=DOTNET");

    let _ = embed_resource::compile("app.manifest", embed_resource::NONE);

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        build_spd_bridge();
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icon.ico")
            .set("ProductName", "内存透镜 Memory Lens")
            .set("FileDescription", "内存可视化分析工具")
            .set("CompanyName", "memory-lens")
            .set("LegalCopyright", "")
            .set("OriginalFilename", "memory-lens.exe")
            .set("InternalName", "memory-lens");
        res.set_version_info(
            winresource::VersionInfo::PRODUCTVERSION,
            0x0000_0001_0000_0000,
        );
        res.set_version_info(
            winresource::VersionInfo::FILEVERSION,
            0x0000_0001_0000_0000,
        );
        if let Err(e) = res.compile() {
            eprintln!("winresource compile failed: {e}");
        }
    }
}

fn build_spd_bridge() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let project = manifest.join("spd-bridge/MemoryLens.SpdBridge.csproj");
    let publish = out.join("spd-bridge-publish");
    let dotnet = std::env::var_os("DOTNET").unwrap_or_else(|| "dotnet".into());

    let status = Command::new(&dotnet)
        .arg("restore")
        .arg(&project)
        .args(["--locked-mode", "--nologo"])
        .current_dir(&manifest)
        .status()
        .expect("无法启动 dotnet；构建 SPD 桥接需要 .NET SDK");
    assert!(status.success(), "dotnet restore --locked-mode 失败");

    let status = Command::new(&dotnet)
        .arg("publish")
        .arg(&project)
        .args(["-c", "Release", "--nologo", "--no-restore", "-o"])
        .arg(&publish)
        .current_dir(&manifest)
        .status()
        .expect("无法启动 dotnet publish");
    assert!(status.success(), "dotnet publish SPD 桥接失败");

    let mut files = Vec::new();
    collect_files(&publish, &publish, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut generated = String::from("pub static SPD_BRIDGE_FILES: &[(&str, &[u8])] = &[\n");
    for (name, path) in files {
        generated.push_str(&format!("    ({name:?}, include_bytes!({:?})),\n", path.to_string_lossy()));
    }
    generated.push_str("];\n");
    fs::write(out.join("spd_bridge_files.rs"), generated).unwrap();
}

fn collect_files(root: &Path, directory: &Path, files: &mut Vec<(String, PathBuf)>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_files(root, &path, files);
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "pdb") {
            continue;
        }
        let name = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        files.push((name, path));
    }
}
