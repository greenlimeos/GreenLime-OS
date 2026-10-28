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
    // M15.1: userland builds run through the SSE-baked target spec (the
    // Makefile's UE_TARGET); its build dir sits under the spec's own name.
    // GREENLIME_UE_DIR names it explicitly; bare `cargo build` falls back
    // to the stock soft-float triple so a plain userland build still works.
    let ue_dir = std::env::var("GREENLIME_UE_DIR")
        .unwrap_or_else(|_| String::from("x86_64-unknown-none"));
    let init = format!(
        "{}/{}/{}/greenlime-init",
        tdir, ue_dir, profile
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
        "{}/{}/{}/greenlime-shell",
        tdir, ue_dir, profile
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
        "{}/{}/{}/greenlime-demo",
        tdir, ue_dir, profile
    );
    println!("cargo:rerun-if-changed={}", demo);
    if !std::path::Path::new(&demo).exists() {
        panic!(
            "greenlime-demo ELF missing at {}. Build userland first: run `make`, not bare `cargo build`.",
            demo
        );
    }
    println!("cargo:rustc-env=GREENLIME_DEMO_ELF={}", demo);

    // Fuzzer ELF, same deal (v1.0 hardening: spawned unprivileged).
    let fuzz = format!(
        "{}/{}/{}/greenlime-fuzz",
        tdir, ue_dir, profile
    );
    println!("cargo:rerun-if-changed={}", fuzz);
    if !std::path::Path::new(&fuzz).exists() {
        panic!(
            "greenlime-fuzz ELF missing at {}. Build userland first: run `make`, not bare `cargo build`.",
            fuzz
        );
    }
    println!("cargo:rustc-env=GREENLIME_FUZZ_ELF={}", fuzz);

    // Wallpaper service ELF, same deal (task 8, CAP_WALL: streams the
    // desktop image to winsrv).
    let wallp = format!(
        "{}/{}/{}/greenlime-wallpaper",
        tdir, ue_dir, profile
    );
    println!("cargo:rerun-if-changed={}", wallp);
    if !std::path::Path::new(&wallp).exists() {
        panic!(
            "greenlime-wallpaper ELF missing at {}. Build userland first: run `make`, not bare `cargo build`.",
            wallp
        );
    }
    println!("cargo:rustc-env=GREENLIME_WALLPAPER_ELF={}", wallp);

    // Notepad ELF, same deal (M18: spawned on demand — right-click New
    // text file / .txt double-click — never at boot).
    let notepad = format!(
        "{}/{}/{}/greenlime-notepad",
        tdir, ue_dir, profile
    );
    println!("cargo:rerun-if-changed={}", notepad);
    if !std::path::Path::new(&notepad).exists() {
        panic!(
            "greenlime-notepad ELF missing at {}. Build userland first: run `make`, not bare `cargo build`.",
            notepad
        );
    }
    println!("cargo:rustc-env=GREENLIME_NOTEPAD_ELF={}", notepad);

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

    // M16.x UI icons: raw straight-alpha RGBA blobs baked ONCE by
    // scripts/bake-icons.py from the committed icons8 PNG sources
    // (assets/icons-src). The kernel never parses image data — these
    // are plain byte arrays winsrv alpha-blits at exactly their given
    // size, no runtime scaling or decode.
    let icons = format!("{}/../assets/icons", manifest);
    const ICONS: [&str; 14] = [
        "folder_24", "folder_28", "folder_30",
        "console_24", "console_28",
        "settings_24", "settings_28",
        "monitor_20", "docsfold_20", "dload_20", "bin_20", "page_28",
        "rocket_28", "power_24",
    ];
    for name in ICONS {
        let p = format!("{}/{}.rgba", icons, name);
        if !std::path::Path::new(&p).exists() {
            panic!(
                "icon blob missing at {}.\nRun scripts/bake-icons.py (network fetches assets/icons-src PNGs once).",
                p
            );
        }
    }
    println!("cargo:rerun-if-changed={}", icons);
    println!("cargo:rustc-env=GREENLIME_ICONS_DIR={}", icons);
}
