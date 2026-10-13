//! L5.
pub use core::primitive::u8 as Octet;
pub use u16 as Word;
mod m {
    pub use core::primitive::u32;
}
pub use m::u32 as DWord;
// Primitive re-exports spelled through local paths (`id: null` too).
mod q {
    pub use core::primitive::u8 as Octet;
}
pub use crate::q::Octet as Byte;
pub use self::q::Octet as Small;
