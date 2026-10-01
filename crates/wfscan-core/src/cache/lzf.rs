//! LZF, the compression Warframe used before Oodle and still uses for the odd block.
//!
//! The stream is a run of tokens, each starting with a control byte `c`:
//! - `c < 32`: `c + 1` literal bytes follow.
//! - otherwise a back-reference: length `(c >> 5) + 2`, or `next byte + 9` when `c >> 5`
//!   is 7; then one more byte, which with `c & 0x1f` as the high bits gives the distance
//!   minus 1.
//!
//! Source: the published LZF format (Marc Lehmann's liblzf), which LotusLib's README
//! names as the "LZ" in these caches.

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LzfError {
    #[error("input ends inside a token")]
    Truncated,
    #[error("back-reference reaches before the start of the output")]
    BadDistance,
    #[error("output is larger than expected")]
    TooLong,
    #[error("output is {0} bytes shorter than expected")]
    TooShort(usize),
}

/// Decode all of `input` into exactly `out.len()` bytes.
pub fn decompress(input: &[u8], out: &mut [u8]) -> Result<(), LzfError> {
    let (mut ip, mut op) = (0, 0);
    while ip < input.len() {
        let ctrl = usize::from(input[ip]);
        ip += 1;
        if ctrl < 32 {
            let n = ctrl + 1;
            let lit = input.get(ip..ip + n).ok_or(LzfError::Truncated)?;
            out.get_mut(op..op + n).ok_or(LzfError::TooLong)?.copy_from_slice(lit);
            ip += n;
            op += n;
        } else {
            let mut len = ctrl >> 5;
            if len == 7 {
                len += usize::from(*input.get(ip).ok_or(LzfError::Truncated)?);
                ip += 1;
            }
            len += 2;
            let low = usize::from(*input.get(ip).ok_or(LzfError::Truncated)?);
            ip += 1;
            let distance = ((ctrl & 0x1f) << 8 | low) + 1;
            if distance > op {
                return Err(LzfError::BadDistance);
            }
            if op + len > out.len() {
                return Err(LzfError::TooLong);
            }
            // The source may overlap the bytes being written (a run), so copy forwards.
            for i in op..op + len {
                out[i] = out[i - distance];
            }
            op += len;
        }
    }
    if op < out.len() {
        return Err(LzfError::TooShort(out.len() - op));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(input: &[u8], len: usize) -> Result<Vec<u8>, LzfError> {
        let mut out = vec![0; len];
        decompress(input, &mut out).map(|()| out)
    }

    #[test]
    fn literals_and_back_references() {
        assert_eq!(decode(&[2, b'a', b'b', b'c'], 3).unwrap(), b"abc");
        // "abc", then 3 bytes from distance 3.
        assert_eq!(decode(&[2, b'a', b'b', b'c', 1 << 5, 2], 6).unwrap(), b"abcabc");
        // A run: one literal, then 10 bytes from distance 1 (long length form: 7 + 1 + 2).
        assert_eq!(decode(&[0, b'z', 7 << 5, 1, 0], 11).unwrap(), b"zzzzzzzzzzz");
        assert_eq!(decode(&[], 0).unwrap(), b"");
    }

    #[test]
    fn far_distance_uses_the_high_bits() {
        let mut input = Vec::new();
        let mut expected = Vec::new();
        for chunk in 0..10u8 {
            input.push(31);
            input.extend(std::iter::repeat_n(chunk, 32));
            expected.extend(std::iter::repeat_n(chunk, 32));
        }
        // 4 bytes from distance 300 = (1 << 8 | 43) + 1.
        input.extend_from_slice(&[2 << 5 | 1, 43]);
        let from = expected.len() - 300;
        expected.extend_from_within(from..from + 4);
        assert_eq!(decode(&input, expected.len()).unwrap(), expected);
    }

    #[test]
    fn errors() {
        assert_eq!(decode(&[2, b'a'], 3), Err(LzfError::Truncated));
        assert_eq!(decode(&[1 << 5], 3), Err(LzfError::Truncated));
        assert_eq!(decode(&[7 << 5], 3), Err(LzfError::Truncated));
        assert_eq!(decode(&[1 << 5, 0], 3), Err(LzfError::BadDistance));
        assert_eq!(decode(&[0, b'a', 1 << 5, 5], 4), Err(LzfError::BadDistance));
        assert_eq!(decode(&[2, b'a', b'b', b'c'], 2), Err(LzfError::TooLong));
        assert_eq!(decode(&[0, b'a', 1 << 5, 0], 3), Err(LzfError::TooLong));
        assert_eq!(decode(&[0, b'a'], 3), Err(LzfError::TooShort(2)));
    }
}
