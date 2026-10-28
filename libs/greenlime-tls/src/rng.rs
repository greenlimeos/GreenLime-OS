//! src/rng.rs — the `rand_core` adapter over the kernel DRBG (M20).
//!
//! Upstream fed `getrandom::SysRng` to the keygen/signature APIs; after
//! the GreenLime swap the same bytes come from the kernel's ChaCha8 DRBG
//! via `Syscall::Rand` (src/sys.rs). The trait shape mirrors upstream
//! exactly: a fallible generator wrapped in `rand_core::UnwrapErr`, so
//! the infallible face the dalek/RustCrypto APIs expect falls back to
//! the loud failure (panic = abort), NEVER to silent weak keys — a key
//! the attacker can predict is worse than a dead connection.

use rand_core::{TryCryptoRng, TryRng};

/// Why a draw was refused. Only reachable before kernel crypto init (or
/// a wedged gate); the infallible face turns this into a loud abort.
#[derive(Debug)]
pub struct RandRefused;

impl core::fmt::Display for RandRefused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("kernel DRBG refused (Syscall::Rand)")
    }
}

impl core::error::Error for RandRefused {}

/// The fallible face: one syscall lane, zero buffering.
pub struct KernelRngTry;

impl TryRng for KernelRngTry {
    type Error = RandRefused;

    fn try_next_u32(&mut self) -> Result<u32, RandRefused> {
        let mut b = [0u8; 4];
        self.try_fill_bytes(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn try_next_u64(&mut self) -> Result<u64, RandRefused> {
        let mut b = [0u8; 8];
        self.try_fill_bytes(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), RandRefused> {
        crate::sys::rand_fill(dst).map_err(|_| RandRefused)
    }
}

impl TryCryptoRng for KernelRngTry {}
