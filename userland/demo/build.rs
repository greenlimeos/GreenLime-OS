//! Demo build script: its own linker script (NOT the kernel's).

fn main() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rustc-link-arg=-T{}/linker.ld", manifest);
}
