//! Arch isolation. All CPU-specific code lives under arch/<arch>/.
//! Rest of kernel calls arch::init(), arch::halt_loop(), arch::interrupts_*().

#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(target_arch = "x86_64")]
pub use self::x86_64::*;

#[cfg(not(target_arch = "x86_64"))]
compile_error!("GreenLime currently supports only x86_64. Add arch/aarch64/ for ARM.");
