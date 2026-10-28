//! greenlime-abi::app — the .gle v2 asset-window contract (M19), shared
//! by the kernel's exec path (userspace/elf.rs) and the greenlime-client
//! asset helpers. Memory-mapped contract, not a wire message: ABI_VERSION
//! stays put.
//!
//! THE WINDOW: every ring-3 task owns one. At exec the kernel maps a
//! single descriptor page at ASSET_BASE (read-only, no-execute); then,
//! for a v2 container, it maps every asset blob read-only, page-aligned,
//! ascending above it. Window contents never change for the life of the
//! task — a client may hold `&'static [u8]` views into it.
//!
//! Descriptor page layout (little-endian; page == 4096 B):
//!   [0..4)   magic "GLA1"
//!   [4..6)   u16 format version (ASSET_VER)
//!   [6..8)   u16 count (0 = no assets; boot images and v1 .gle files
//!            get an empty page with the identical header)
//!   [8..8+16*count)  descriptors, ascending:
//!     { u64 vaddr — user virtual address of the mapped blob,
//!       u64 len   — exact byte length }
//!   (the rest of the page stays zero)
//!
//! Blobs map from ASSET_BASE + 4096 upward, in descriptor order, each
//! page-aligned. A blob of L bytes occupies ceil(L / 4096) pages beyond
//! `len`; clients never read past `len`.
//!
//! The window base sits far above every static user link (all built-in
//! images link <= 0x300000) and far below the user-half boundary; the
//! ELF loader refuses any program segment inside the window, so an app
//! can never shadow its own assets. The user stack (grown down from
//! 0x7FFF_FFFF_F000) is higher still.

/// Window descriptor magic.
pub const ASSET_MAGIC: [u8; 4] = *b"GLA1";
/// Descriptor-page format version.
pub const ASSET_VER: u16 = 1;
/// Every task's window base (4 KiB-aligned).
pub const ASSET_BASE: u64 = 0x0000_0060_0000_0000;
/// Window size: an 8 MiB container maps with room to spare (per-blob
/// alignment + the descriptor page); the parse-time caps (container
/// <= 8 MiB, <= 32 entries) bind the math tighter still.
pub const ASSET_WINDOW: u64 = 16 * 1024 * 1024;
/// Descriptor-page capacity: (4096 - 8) / 16. The container law caps
/// entries at 32, so this page capacity is not the binder in practice.
pub const ASSET_MAX: usize = (4096 - 8) / 16;
