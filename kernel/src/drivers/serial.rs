//! Serial driver (COM1 0x3F8). Backs kprint. Init early, never fails.
use crate::kprint;

pub fn init() {
    kprint::init();
}
