//! Kernel build script: linker script + embedded init ELF path.

fn main() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rustc-link-arg=-T{}/linker.ld", manifest);

    // PID 1 must be built first (`make` does this). Fail loudly otherwise.
    // Profile-aware: release kernel embeds release userland. PROFILE is set
    // for build-script *execution* (not compilation), so read at runtime.
    // Same for CARGO_TARGET_DIR: the Makefile exports it (cache lives in
    // ~/.cache so the tree stays lean); fall back to the in-tree default
    // so bare `cargo build` keeps working.
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| String::from("debug"));
    let tdir = std::env::var("CARGO_TARGET_DIR")
        .unwrap_or_else(|_| format!("{}/../target", manifest));
    let init = format!(
        "{}/x86_64-unknown-none/{}/greenlime-init",
        tdir, profile
    );
    println!("cargo:rerun-if-changed={}", init);
    if !std::path::Path::new(&init).exists() {
        panic!(
            "greenlime-init ELF missing at {}. Build userland first: run `make`, not bare `cargo build`.",
            init
        );
    }
    println!("cargo:rustc-env=GREENLIME_INIT_ELF={}", init);

    // Shell ELF, same deal (see spawn_all).
    let shell = format!(
        "{}/x86_64-unknown-none/{}/greenlime-shell",
        tdir, profile
    );
    println!("cargo:rerun-if-changed={}", shell);
    if !std::path::Path::new(&shell).exists() {
        panic!(
            "greenlime-shell ELF missing at {}. Build userland first: run `make`, not bare `cargo build`.",
            shell
        );
    }
    println!("cargo:rustc-env=GREENLIME_SHELL_ELF={}", shell);

    // Demo window client ELF, same deal (see spawn_all).
    let demo = format!(
        "{}/x86_64-unknown-none/{}/greenlime-demo",
        tdir, profile
    );
    println!("cargo:rerun-if-changed={}", demo);
    if !std::path::Path::new(&demo).exists() {
        panic!(
            "greenlime-demo ELF missing at {}. Build userland first: run `make`, not bare `cargo build`.",
            demo
        );
    }
    println!("cargo:rustc-env=GREENLIME_DEMO_ELF={}", demo);

    // Default TTF for the framebuffer console (see assets/fonts/README.md).
    let font = format!("{}/../assets/fonts/default-font.ttf", manifest);
    println!("cargo:rerun-if-changed={}", font);
    if !std::path::Path::new(&font).exists() {
        panic!(
            "default font missing at {}.\n\
             Put any TrueType (.ttf) font there as exactly `default-font.ttf`.\n\
             See assets/fonts/README.md for free OFL options (DejaVu, Inter, Fira, Plex).",
            font
        );
    }
    println!("cargo:rustc-env=GREENLIME_FONT_TTF={}", font);
}
