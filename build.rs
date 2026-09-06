fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Pack relative-address fixups rather than changing generated code. The
    // GNU linker records GLIBC_ABI_DT_RELR as a runtime requirement (2.36+).
    // Omarchy's x86-64 glibc platform supports this compact ELF format.
    if std::env::var("PROFILE").as_deref() == Ok("release")
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu")
    {
        println!("cargo:rustc-link-arg=-Wl,-z,pack-relative-relocs");
    }
}
