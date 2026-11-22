//! How protocol values become bytes: the [`Wire`] trait for the types, and
//! [`wire_struct!`] and [`wire_enum!`] for structs and enums made of them.
//!
//! * Integers are little-endian; `i32` is two's complement.
//! * A `bool` is one byte, 0 or 1.
//! * An `Option<T>` is one byte, 0 (none) or 1, then the value if there is one.
//! * A string is a `u16` count of bytes, then that many bytes of UTF-8. A list is a
//!   `u16` count of items, then the items. Every string and list field states the most
//!   it may hold ([`Limited`]).
//! * A `[u8; N]` is N bytes, with no count.
//! * A struct is its fields in declaration order, with nothing in between.
//! * An enum is a `u8` tag written next to each variant, then that variant's fields.
//!   Variants may be reordered without changing a byte.
//!
//! Decoding is strict: bytes left over, a `bool` or `Option` byte other than 0 or 1,
//! a tag no variant has, a string that is not UTF-8 and a string or list over its
//! limit are all errors. Encoding fails only for a string or list over its limit.

use std::fmt;

/// Why bytes are not a valid value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// The bytes end in the middle of a value.
    Truncated,
    /// Bytes are left over after the value.
    Trailing(usize),
    /// A `bool` byte other than 0 or 1.
    BadBool(u8),
    /// A string that is not UTF-8.
    BadUtf8,
    /// A string or list longer than its field allows.
    TooLong { len: usize, max: usize },
    /// A tag that no variant of `what` has.
    UnknownTag { what: &'static str, tag: u8 },
    /// An id listed twice where each may be listed once.
    Repeated(u32),
    /// A value the format allows but the protocol does not (a size of nothing, a
    /// level over 100 %, a reference to a display that is not listed).
    Invalid(&'static str),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(f, "message cut short"),
            Self::Trailing(n) => write!(f, "{n} bytes left over after the message"),
            Self::BadBool(b) => write!(f, "{b} is not a boolean"),
            Self::BadUtf8 => write!(f, "text is not UTF-8"),
            Self::TooLong { len, max } => write!(f, "{len} items where at most {max} fit"),
            Self::UnknownTag { what, tag } => write!(f, "unknown {what} {tag}"),
            Self::Repeated(id) => write!(f, "{id} listed twice"),
            Self::Invalid(what) => write!(f, "invalid value: {what}"),
        }
    }
}

impl std::error::Error for WireError {}

impl From<WireError> for std::io::Error {
    fn from(e: WireError) -> Self {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    }
}

/// Reads values off the front of a byte slice.
#[derive(Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    /// The next `n` bytes.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if self.buf.len() < n {
            return Err(WireError::Truncated);
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    /// The next `N` bytes.
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let (head, tail) = self
            .buf
            .split_first_chunk::<N>()
            .ok_or(WireError::Truncated)?;
        self.buf = tail;
        Ok(*head)
    }

    /// Everything that is left.
    pub fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.buf)
    }

    pub fn remaining(&self) -> usize {
        self.buf.len()
    }

    /// Checks that nothing is left.
    pub fn finish(self) -> Result<(), WireError> {
        match self.buf.len() {
            0 => Ok(()),
            n => Err(WireError::Trailing(n)),
        }
    }
}

/// A value with a fixed way to be written and read. Writing fails only for a string
/// or list over its limit inside the value.
pub trait Wire: Sized {
    fn put(&self, out: &mut Vec<u8>) -> Result<(), WireError>;
    fn get(r: &mut Reader<'_>) -> Result<Self, WireError>;
}

/// A string or a list: a `u16` count, then the items. Each field of such a type states
/// the most it may hold (`#[wire(max = N)]` in [`wire_struct!`] and [`wire_enum!`]), so
/// a field without a limit does not compile.
pub trait Limited: Sized {
    /// Fails with [`WireError::TooLong`] for more than `max` (or a `u16`) holds.
    fn put(&self, out: &mut Vec<u8>, max: usize) -> Result<(), WireError>;
    fn get(r: &mut Reader<'_>, max: usize) -> Result<Self, WireError>;
}

/// The last field of a message, running to its end with no count (`#[wire(rest)]`).
/// Only the frozen version handshake and the media messages use it.
pub trait Rest: Sized {
    fn put(&self, out: &mut Vec<u8>) -> Result<(), WireError>;
    fn get(r: &mut Reader<'_>) -> Result<Self, WireError>;
}

/// The bytes of `value`.
pub fn encode<T: Wire>(value: &T) -> Result<Vec<u8>, WireError> {
    let mut out = Vec::new();
    value.put(&mut out)?;
    Ok(out)
}

/// The value that `bytes` hold, all of them.
pub fn decode<T: Wire>(bytes: &[u8]) -> Result<T, WireError> {
    let mut r = Reader::new(bytes);
    let value = T::get(&mut r)?;
    r.finish()?;
    Ok(value)
}

macro_rules! int {
    ($($t:ty),*) => {$(
        impl Wire for $t {
            fn put(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
                out.extend_from_slice(&self.to_le_bytes());
                Ok(())
            }
            fn get(r: &mut Reader<'_>) -> Result<Self, WireError> {
                Ok(<$t>::from_le_bytes(r.array()?))
            }
        }
    )*};
}

int!(u8, i8, u16, u32, u64, i32);

impl Wire for bool {
    fn put(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        out.push(u8::from(*self));
        Ok(())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, WireError> {
        match u8::get(r)? {
            0 => Ok(false),
            1 => Ok(true),
            b => Err(WireError::BadBool(b)),
        }
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        match self {
            None => {
                out.push(0);
                Ok(())
            }
            Some(v) => {
                out.push(1);
                v.put(out)
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, WireError> {
        match u8::get(r)? {
            0 => Ok(None),
            1 => Ok(Some(T::get(r)?)),
            tag => Err(WireError::UnknownTag {
                what: "Option",
                tag,
            }),
        }
    }
}

impl<const N: usize> Wire for [u8; N] {
    fn put(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        out.extend_from_slice(self);
        Ok(())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, WireError> {
        r.array()
    }
}

impl<A: Wire, B: Wire> Wire for (A, B) {
    fn put(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        self.0.put(out)?;
        self.1.put(out)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok((A::get(r)?, B::get(r)?))
    }
}

fn put_count(out: &mut Vec<u8>, len: usize, max: usize) -> Result<(), WireError> {
    let max = max.min(usize::from(u16::MAX));
    if len > max {
        return Err(WireError::TooLong { len, max });
    }
    (len as u16).put(out)
}

fn get_count(r: &mut Reader<'_>, max: usize) -> Result<usize, WireError> {
    let len = usize::from(u16::get(r)?);
    if len > max {
        return Err(WireError::TooLong { len, max });
    }
    Ok(len)
}

impl Limited for String {
    fn put(&self, out: &mut Vec<u8>, max: usize) -> Result<(), WireError> {
        put_count(out, self.len(), max)?;
        out.extend_from_slice(self.as_bytes());
        Ok(())
    }
    fn get(r: &mut Reader<'_>, max: usize) -> Result<Self, WireError> {
        let len = get_count(r, max)?;
        let bytes = r.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| WireError::BadUtf8)
    }
}

impl<T: Wire> Limited for Vec<T> {
    fn put(&self, out: &mut Vec<u8>, max: usize) -> Result<(), WireError> {
        put_count(out, self.len(), max)?;
        self.iter().try_for_each(|item| item.put(out))
    }
    fn get(r: &mut Reader<'_>, max: usize) -> Result<Self, WireError> {
        let len = get_count(r, max)?;
        // Every item takes at least a byte: a count the bytes cannot back allocates
        // no more than what is there.
        let mut items = Vec::with_capacity(len.min(r.remaining()));
        for _ in 0..len {
            items.push(T::get(r)?);
        }
        Ok(items)
    }
}

impl Rest for Vec<u8> {
    fn put(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        out.extend_from_slice(self);
        Ok(())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(r.rest().to_vec())
    }
}

impl Rest for String {
    fn put(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        out.extend_from_slice(self.as_bytes());
        Ok(())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, WireError> {
        String::from_utf8(r.rest().to_vec()).map_err(|_| WireError::BadUtf8)
    }
}

/// Fails to compile when two tags are the same.
#[doc(hidden)]
pub const fn assert_distinct(tags: &[u8]) {
    let mut i = 0;
    while i < tags.len() {
        let mut j = i + 1;
        while j < tags.len() {
            assert!(tags[i] != tags[j], "two variants have the same tag");
            j += 1;
        }
        i += 1;
    }
}

/// Writes one field: plain, with a limit (`max = N`) or to the end (`rest`).
macro_rules! put_field {
    ($out:expr, $value:expr, $ty:ty;) => {
        <$ty as $crate::wire::Wire>::put($value, $out)
    };
    ($out:expr, $value:expr, $ty:ty; max = $max:expr) => {
        <$ty as $crate::wire::Limited>::put($value, $out, $max)
    };
    ($out:expr, $value:expr, $ty:ty; rest) => {
        <$ty as $crate::wire::Rest>::put($value, $out)
    };
    ($out:expr, $value:expr, $ty:ty; skip) => {{
        let _ = $value;
        Ok::<(), $crate::wire::WireError>(())
    }};
}

/// Reads one field, as [`put_field!`] writes it.
macro_rules! get_field {
    ($r:expr, $ty:ty;) => {
        <$ty as $crate::wire::Wire>::get($r)?
    };
    ($r:expr, $ty:ty; max = $max:expr) => {
        <$ty as $crate::wire::Limited>::get($r, $max)?
    };
    ($r:expr, $ty:ty; rest) => {
        <$ty as $crate::wire::Rest>::get($r)?
    };
    ($r:expr, $ty:ty; skip) => {
        <$ty as Default>::default()
    };
}

/// A struct whose bytes are its fields in declaration order. A string or list field
/// takes `#[wire(max = N)]` after its doc comment; a field marked `#[wire(skip)]` is
/// not in the bytes at all (it is read as its default), for what travels in a message
/// of its own.
///
/// ```ignore
/// wire_struct! {
///     #[derive(Debug, Clone, PartialEq)]
///     pub struct Monitor {
///         pub id: u8,
///         /// Shown in the menu.
///         #[wire(max = 64)]
///         pub name: String,
///     }
/// }
/// ```
macro_rules! wire_struct {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $(
                $(#[doc = $doc:literal])*
                $(#[wire($($mode:tt)*)])?
                $fvis:vis $field:ident : $ty:ty
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis struct $name {
            $( $(#[doc = $doc])* $fvis $field: $ty, )*
        }

        impl $crate::wire::Wire for $name {
            fn put(&self, out: &mut Vec<u8>) -> Result<(), $crate::wire::WireError> {
                let Self { $($field),* } = self;
                $( $crate::wire::put_field!(out, $field, $ty; $($($mode)*)?)?; )*
                Ok(())
            }

            fn get(
                r: &mut $crate::wire::Reader<'_>,
            ) -> Result<Self, $crate::wire::WireError> {
                Ok(Self {
                    $( $field: $crate::wire::get_field!(r, $ty; $($($mode)*)?), )*
                })
            }
        }
    };
}

/// An enum whose bytes are the tag written next to each variant, then the variant's
/// fields in declaration order (as in [`wire_struct!`]). The tags are explicit, so
/// reordering variants never changes the bytes; two variants with one tag do not
/// compile.
///
/// ```ignore
/// wire_enum! {
///     #[derive(Debug, Clone, Copy, PartialEq, Eq)]
///     pub enum Shape {
///         1 => Square,
///         2 => Rect { width: u32, height: u32 },
///     }
/// }
/// ```
///
/// `wire_enum! { message ... }` defines a message enum instead: the same tags and
/// fields, but no [`Wire`] impl, since messages are framed with a length and an unknown
/// one is skipped rather than refused (see the crate root).
macro_rules! wire_enum {
    (@define
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[doc = $vdoc:literal])*
                $tag:literal => $variant:ident $({
                    $(
                        $(#[doc = $doc:literal])*
                        $(#[wire($($mode:tt)*)])?
                        $field:ident : $ty:ty
                    ),* $(,)?
                })?
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis enum $name {
            $(
                $(#[doc = $vdoc])*
                $variant $({ $( $(#[doc = $doc])* $field: $ty, )* })?,
            )*
        }

        const _: () = $crate::wire::assert_distinct($name::TAGS);

        impl $name {
            /// The tag of every variant.
            pub(crate) const TAGS: &'static [u8] = &[$($tag),*];

            pub(crate) fn wire_tag(&self) -> u8 {
                match self {
                    $( Self::$variant { .. } => $tag, )*
                }
            }

            // An enum without fields in any variant writes nothing here.
            #[allow(unused_variables, clippy::ptr_arg)]
            pub(crate) fn put_fields(&self, out: &mut Vec<u8>) -> Result<(), $crate::wire::WireError> {
                match self {
                    $(
                        Self::$variant $({ $($field),* })? => {
                            $($( $crate::wire::put_field!(out, $field, $ty; $($($mode)*)?)?; )*)?
                        }
                    )*
                }
                Ok(())
            }

            /// The variant with `tag`, its fields read from `r`; `None` for a tag no
            /// variant has.
            #[allow(unused_variables)]
            pub(crate) fn get_fields(
                tag: u8,
                r: &mut $crate::wire::Reader<'_>,
            ) -> Result<Option<Self>, $crate::wire::WireError> {
                Ok(Some(match tag {
                    $(
                        $tag => Self::$variant $({
                            $( $field: $crate::wire::get_field!(r, $ty; $($($mode)*)?), )*
                        })?,
                    )*
                    _ => return Ok(None),
                }))
            }
        }
    };
    (message $($def:tt)*) => {
        $crate::wire::wire_enum!(@define $($def)*);
    };
    ($(#[$meta:meta])* $vis:vis enum $name:ident { $($body:tt)* }) => {
        $crate::wire::wire_enum!(@define $(#[$meta])* $vis enum $name { $($body)* });

        impl $crate::wire::Wire for $name {
            fn put(&self, out: &mut Vec<u8>) -> Result<(), $crate::wire::WireError> {
                out.push(self.wire_tag());
                self.put_fields(out)
            }

            fn get(
                r: &mut $crate::wire::Reader<'_>,
            ) -> Result<Self, $crate::wire::WireError> {
                let tag = <u8 as $crate::wire::Wire>::get(r)?;
                Self::get_fields(tag, r)?.ok_or($crate::wire::WireError::UnknownTag {
                    what: stringify!($name),
                    tag,
                })
            }
        }
    };
}

pub(crate) use {get_field, put_field, wire_enum, wire_struct};

#[cfg(test)]
mod tests {
    use super::*;

    wire_struct! {
        /// A struct for the tests.
        #[derive(Debug, Clone, PartialEq)]
        struct Sample {
            id: u8,
            /// Documented, with a limit.
            #[wire(max = 8)]
            name: String,
            #[wire(max = 3)]
            sizes: Vec<(u32, u32)>,
            seen: Option<i32>,
            key: [u8; 4],
            on: bool,
            shape: Shape,
        }
    }

    wire_enum! {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Shape {
            /// Listed out of tag order: the tags decide the bytes.
            7 => Rect { width: u16, height: u16 },
            1 => Square,
        }
    }

    fn sample() -> Sample {
        Sample {
            id: 3,
            name: "ab".into(),
            sizes: vec![(1, 2)],
            seen: Some(-2),
            key: [9, 8, 7, 6],
            on: true,
            shape: Shape::Rect {
                width: 5,
                height: 6,
            },
        }
    }

    #[test]
    fn fields_in_order_with_explicit_tags() {
        let bytes = encode(&sample()).unwrap();
        #[rustfmt::skip]
        let expected = [
            3,                       // id
            2, 0, b'a', b'b',        // name: count, bytes
            1, 0, 1, 0, 0, 0, 2, 0, 0, 0, // sizes: count, (1, 2)
            1, 0xfe, 0xff, 0xff, 0xff,    // seen: Some(-2)
            9, 8, 7, 6,              // key
            1,                       // on
            7, 5, 0, 6, 0,           // shape: tag 7, width, height
        ];
        assert_eq!(bytes, expected);
        assert_eq!(decode::<Sample>(&bytes), Ok(sample()));
        assert_eq!(encode(&Shape::Square).unwrap(), [1]);
        assert_eq!(encode(&None::<u8>).unwrap(), [0]);
    }

    #[test]
    fn decoding_is_strict() {
        let good = encode(&sample()).unwrap();
        let mut longer = good.clone();
        longer.push(0);
        assert_eq!(decode::<Sample>(&longer), Err(WireError::Trailing(1)));
        assert_eq!(
            decode::<Sample>(&good[..good.len() - 1]),
            Err(WireError::Truncated)
        );
        assert_eq!(decode::<bool>(&[2]), Err(WireError::BadBool(2)));
        assert_eq!(
            decode::<Option<u8>>(&[2, 0]),
            Err(WireError::UnknownTag {
                what: "Option",
                tag: 2
            })
        );
        assert_eq!(
            decode::<Shape>(&[2]),
            Err(WireError::UnknownTag {
                what: "Shape",
                tag: 2
            })
        );
        let utf8 = |bytes: &[u8]| {
            let mut s = sample();
            s.name = String::new();
            let mut b = encode(&s).unwrap();
            // The name starts after the id.
            b.splice(
                1..3,
                [bytes.len() as u8, 0]
                    .into_iter()
                    .chain(bytes.iter().copied()),
            );
            decode::<Sample>(&b)
        };
        assert_eq!(utf8(&[0xff, 0xfe]), Err(WireError::BadUtf8));
        assert!(utf8("ї".as_bytes()).is_ok());
    }

    #[test]
    fn limits_hold() {
        let mut s = sample();
        s.name = "x".repeat(9);
        // A sender cannot write it.
        assert_eq!(encode(&s), Err(WireError::TooLong { len: 9, max: 8 }));
        // Written by hand.
        let mut b = vec![3, 9, 0];
        b.extend_from_slice(s.name.as_bytes());
        assert_eq!(
            decode::<Sample>(&b),
            Err(WireError::TooLong { len: 9, max: 8 })
        );
        // A count far beyond the bytes fails without allocating for it.
        let mut r = Reader::new(&[0xff, 0xff, 1]);
        assert_eq!(
            <Vec<u8> as Limited>::get(&mut r, usize::from(u16::MAX)),
            Err(WireError::Truncated)
        );
        let mut out = Vec::new();
        assert_eq!(
            Limited::put(&vec![0u8; 4], &mut out, 3),
            Err(WireError::TooLong { len: 4, max: 3 })
        );
        // Nor past what the count can say.
        assert_eq!(
            Limited::put(&vec![0u8; 70_000], &mut out, usize::MAX),
            Err(WireError::TooLong {
                len: 70_000,
                max: 65_535
            })
        );
    }

    #[test]
    fn rest_runs_to_the_end() {
        let mut out = Vec::new();
        Rest::put(&"héllo".to_string(), &mut out).unwrap();
        assert_eq!(out, "héllo".as_bytes());
        let mut r = Reader::new(&out);
        assert_eq!(<String as Rest>::get(&mut r).unwrap(), "héllo");
        r.finish().unwrap();
        let mut r = Reader::new(&[0xc3]);
        assert_eq!(<String as Rest>::get(&mut r), Err(WireError::BadUtf8));
    }
}
