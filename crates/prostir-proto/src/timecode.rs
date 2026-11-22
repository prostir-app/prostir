//! A time code drawn into a monitor's picture, for measuring how long a picture takes
//! from the PC to the eye (a test mode only, see PROTOCOL.md, "Measuring the latency
//! from the PC to the eye"). The PC draws it into the frames it captures
//! ([`draw`]); the headset reads it back from what it decoded ([`read`]). Both sides
//! use these functions, so they agree on the format.
//!
//! The code is a row of square cells along the top left of the picture, each
//! [`CELL`] pixels, on the luma plane (in 4:2:0 a cell covers whole chroma samples):
//! a white cell and a black one (where the code starts), then 32 cells of the value,
//! most significant bit first, then 8 cells of a check (the four bytes of the value
//! added, modulo 256), white for 1 and black for 0. The value is the PC's
//! `CLOCK_MONOTONIC` in milliseconds when the frame was captured, its low 32 bits.

/// The side of a cell in pixels.
pub const CELL: usize = 16;
/// Cells in the code: 2 to start, 32 of the value, 8 of the check.
pub const CELLS: usize = 2 + 32 + 8;
/// Luma of a 1 and of a 0 (video range).
pub const WHITE: u8 = 235;
pub const BLACK: u8 = 16;

/// The check of `value`: its four bytes added.
fn check(value: u32) -> u8 {
    value
        .to_le_bytes()
        .iter()
        .fold(0u8, |a, b| a.wrapping_add(*b))
}

/// The cells of `value`, from the left: `true` for white.
fn cells(value: u32) -> [bool; CELLS] {
    let mut out = [false; CELLS];
    out[0] = true;
    for i in 0..32 {
        out[2 + i] = (value >> (31 - i)) & 1 == 1;
    }
    let c = check(value);
    for i in 0..8 {
        out[34 + i] = (c >> (7 - i)) & 1 == 1;
    }
    out
}

/// Draws `value` into a luma plane `width` pixels wide and `height` high, its rows
/// `stride` bytes apart; a picture too small for the code is left as it is.
pub fn draw(luma: &mut [u8], stride: usize, width: usize, height: usize, value: u32) {
    if width < CELLS * CELL || height < CELL || luma.len() < stride * (CELL - 1) + CELLS * CELL {
        return;
    }
    let cells = cells(value);
    for row in luma.chunks_mut(stride).take(CELL) {
        for (cell, white) in row.chunks_mut(CELL).zip(cells) {
            cell.fill(if white { WHITE } else { BLACK });
        }
    }
}

/// Reads the code back from a luma plane, as [`draw`] lays it out (looking at the
/// middle of each cell, so the edges an encoder blurs do not count); `None` where
/// there is no code, or its check does not match (a picture of two frames at once).
pub fn read(luma: &[u8], stride: usize, width: usize, height: usize) -> Option<u32> {
    if width < CELLS * CELL || height < CELL {
        return None;
    }
    let y = CELL / 2;
    let row = luma.get(y * stride..y * stride + CELLS * CELL)?;
    let white = |i: usize| row[i * CELL + CELL / 2] > 128;
    if !white(0) || white(1) {
        return None;
    }
    let value = (0..32).fold(0u32, |v, i| (v << 1) | u32::from(white(2 + i)));
    let c = (0..8).fold(0u8, |v, i| (v << 1) | u8::from(white(34 + i)));
    (c == check(value)).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_reads_back() {
        let (w, h, stride) = (CELLS * CELL + 10, 40, CELLS * CELL + 64);
        for value in [0, 1, 0xdead_beef, u32::MAX, 123_456_789] {
            let mut luma = vec![128u8; stride * h];
            draw(&mut luma, stride, w, h, value);
            assert_eq!(read(&luma, stride, w, h), Some(value));
            // Blurred edges, as an encoder leaves them: still read.
            for row in luma.chunks_mut(stride).take(CELL) {
                for x in (0..CELLS * CELL).step_by(CELL) {
                    row[x] = 128;
                }
            }
            assert_eq!(read(&luma, stride, w, h), Some(value));
        }
    }

    /// The code is read in the middle of its cells: rows at the top and the bottom
    /// that an encoder blurred do not count.
    #[test]
    fn blurred_top_and_bottom_rows_still_read() {
        let (w, h) = (CELLS * CELL, CELL);
        let mut luma = vec![0u8; w * h];
        draw(&mut luma, w, w, h, 0x1234_5678);
        for row in [0, CELL - 1] {
            luma[row * w..(row + 1) * w].fill(128);
        }
        assert_eq!(read(&luma, w, w, h), Some(0x1234_5678));
    }

    /// A plane one byte short of the code's last row is left as it is.
    #[test]
    fn a_plane_too_short_for_the_code_is_untouched() {
        let (w, h) = (CELLS * CELL, CELL);
        let mut luma = vec![7u8; w * (CELL - 1) + CELLS * CELL - 1];
        draw(&mut luma, w, w, h, 42);
        assert!(luma.iter().all(|b| *b == 7));
        let mut whole = vec![7u8; w * (CELL - 1) + CELLS * CELL];
        draw(&mut whole, w, w, h, 42);
        assert_eq!(read(&whole, w, w, h), Some(42));
    }

    #[test]
    fn no_code_or_a_broken_one_reads_as_none() {
        let (w, h) = (CELLS * CELL, CELL);
        let plain = vec![128u8; w * h];
        assert_eq!(read(&plain, w, w, h), None);
        let mut luma = vec![0u8; w * h];
        draw(&mut luma, w, w, h, 42);
        // One bit of the value turned over: the check does not match.
        for row in luma.chunks_mut(w) {
            row[(2 + 31) * CELL..(2 + 32) * CELL].fill(WHITE);
        }
        assert_eq!(read(&luma, w, w, h), None);
        // Too small a picture: nothing drawn, nothing read.
        let mut small = vec![7u8; 100];
        draw(&mut small, 10, 10, 10, 42);
        assert!(small.iter().all(|b| *b == 7));
        assert_eq!(read(&small, 10, 10, 10), None);
    }
}
