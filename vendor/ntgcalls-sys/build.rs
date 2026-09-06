use std::env;
use std::fs;
use std::path::PathBuf;

fn shared_name() -> Option<&'static str> {
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    match (os.as_str(), arch.as_str()) {
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("macos", _) => Some("macos-arm64"),
        ("windows", "x86_64") => Some("windows-x86_64"),
        _ => None,
    }
}

fn download_lib(out: &PathBuf) -> PathBuf {
    let name = shared_name().expect("unsupported target platform");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let base = env::var("NTGCALLS_RELEASE_BASE")
        .unwrap_or_else(|_| "https://github.com/pytgcalls/ntgcalls/releases/download".into());
    let url = format!("{base}/v{version}/ntgcalls.{name}-static_libs.zip");
    let dir = out.join("ntgcalls-lib");
    if !dir.join("lib").exists() {
        let bytes = ureq::get(&url)
            .call()
            .unwrap_or_else(|e| panic!("failed to fetch {url}: {e}"))
            .into_body()
            .into_with_config()
            .limit(u64::MAX)
            .read_to_vec()
            .expect("read release body");
        let reader = std::io::Cursor::new(bytes);
        let mut zip = zip::ZipArchive::new(reader).expect("open zip");
        fs::create_dir_all(&dir).unwrap();
        zip.extract(&dir).expect("extract zip");
    }
    dir.join("lib")
}

fn lib_dir() -> PathBuf {
    if let Ok(dir) = env::var("NTGCALLS_LIB_DIR") {
        return PathBuf::from(dir);
    }
    let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../static-output/lib");
    if local.exists() {
        return local;
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    download_lib(&out)
}

#[cfg(target_env = "gnu")]
fn host_glibc_ge_234() -> bool {
    use std::ffi::CStr;
    use std::os::raw::c_char;
    extern "C" {
        fn gnu_get_libc_version() -> *const c_char;
    }
    let raw = unsafe { CStr::from_ptr(gnu_get_libc_version()) };
    let ver = raw.to_string_lossy();
    let mut parts = ver.split('.');
    let major: u32 = parts
        .next()
        .and_then(|x| x.trim().parse().ok())
        .unwrap_or(0);
    let minor: u32 = parts
        .next()
        .and_then(|x| x.trim().parse().ok())
        .unwrap_or(0);
    major > 2 || (major == 2 && minor >= 34)
}

#[cfg(not(target_env = "gnu"))]
fn host_glibc_ge_234() -> bool {
    false
}

fn main() {
    println!("cargo:rustc-check-cfg=cfg(glibc_resolv_compat)");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu")
        && host_glibc_ge_234()
    {
        println!("cargo:rustc-cfg=glibc_resolv_compat");
    }

    let dir = lib_dir();
    println!("cargo:rerun-if-env-changed=NTGCALLS_LIB_DIR");
    println!("cargo:rerun-if-env-changed=NTGCALLS_DYLIB");
    println!("cargo:rustc-link-search=native={}", dir.display());

    if env::var("NTGCALLS_DYLIB").is_ok() {
        println!("cargo:rustc-link-lib=dylib=ntgcalls");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
        println!("cargo:rustc-link-arg=-Wl,--allow-shlib-undefined");
        return;
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        // Rust groups native static libraries before dynamic libraries. A small
        // GNU linker script keeps the system providers ahead of the archive,
        // so its duplicate GLib/FFmpeg members are never extracted.
        let mut providers = Vec::new();
        for (name, major) in [
            ("libavformat", "63"),
            ("libavcodec", "63"),
            ("libavutil", "61"),
            ("libswresample", "7"),
        ] {
            let library = pkg_config::Config::new()
                .statik(false)
                .cargo_metadata(false)
                .probe(name)
                .unwrap_or_else(|error| {
                    panic!("ntgcalls requires system {name} ABI {major}: {error}")
                });
            assert_eq!(library.version.split('.').next(), Some(major),
                "ntgcalls 3.0.0-rc01 requires {name} ABI {major}; rebuild the engine for a different ABI");
            for lib in &library.libs {
                let path = library
                    .link_paths
                    .iter()
                    .map(|dir| dir.join(format!("lib{lib}.so")))
                    .find(|path| path.exists())
                    .expect("shared FFmpeg library");
                providers.push(path);
            }
        }
        for name in ["gio-2.0", "gobject-2.0", "glib-2.0"] {
            let library = pkg_config::Config::new()
                .atleast_version("2.88")
                .statik(false)
                .cargo_metadata(false)
                .probe(name)
                .unwrap_or_else(|error| panic!("ntgcalls requires system {name}: {error}"));
            for lib in &library.libs {
                let path = library
                    .link_paths
                    .iter()
                    .map(|dir| dir.join(format!("lib{lib}.so")))
                    .find(|path| path.exists())
                    .expect("shared GLib library");
                if !providers.contains(&path) {
                    providers.push(path);
                }
            }
        }
        providers.push(dir.join("libntgcalls.a"));
        let input = providers
            .iter()
            .map(|path| {
                let path = path.to_str().expect("UTF-8 native library path");
                assert!(
                    !path.contains(['"', '\n', '\r']),
                    "unsupported native library path"
                );
                format!("\"{path}\"")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
        fs::write(
            out.join("libomarchy_ntgcalls.so"),
            format!("INPUT (\n{input}\n)\n"),
        )
        .unwrap();
        println!("cargo:rustc-link-search=native={}", out.display());
        println!("cargo:rustc-link-lib=dylib=omarchy_ntgcalls");
    } else {
        println!("cargo:rustc-link-lib=static=ntgcalls");
    }

    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => {
            for lib in ["stdc++", "m", "z", "resolv"] {
                println!("cargo:rustc-link-lib=dylib={lib}");
            }
        }
        Ok("macos") => {
            println!("cargo:rustc-link-lib=dylib=c++");
            println!("cargo:rustc-link-lib=dylib=z");
            println!("cargo:rustc-link-lib=framework=Foundation");
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
        }
        Ok("windows") => {
            for lib in [
                "winmm",
                "ws2_32",
                "strmiids",
                "dmoguids",
                "iphlpapi",
                "msdmo",
                "secur32",
                "wmcodecdspuuid",
                "d3d11",
                "dxgi",
                "dwmapi",
                "shcore",
                "bcrypt",
                "ntdll",
                "crypt32",
                "userenv",
                "gdi32",
                "user32",
            ] {
                println!("cargo:rustc-link-lib=dylib={lib}");
            }
        }
        _ => {}
    }
}
