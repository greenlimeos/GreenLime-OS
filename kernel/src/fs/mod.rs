//! Filesystems: FAT32 — read since v0.x, writable since M14 (v1.1).
//! Chain alloc/free, dir entry create/delete, mkdir, write/extend/
//! truncate, all behind the single vfs task. More later.

pub mod fat32;
