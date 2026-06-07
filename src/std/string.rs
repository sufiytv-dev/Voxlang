use crate::std::collections::Vec;
use core::fmt;
use core::ops::{Deref, DerefMut};
use core::str;

/// A UTF‑8 encoded, growable string.
#[derive(Clone, Default, Debug)]
pub struct String {
    vec: Vec<u8>,
}

impl String {
    #[inline]
    pub fn new() -> Self {
        String { vec: Vec::new() }
    }

    pub fn from(s: &str) -> Self {
        let mut vec = Vec::with_capacity(s.len());
        vec.extend_from_slice(s.as_bytes());
        String { vec }
    }

    pub fn push_str(&mut self, s: &str) {
        self.vec.extend_from_slice(s.as_bytes());
    }

    pub fn as_str(&self) -> &str {
        unsafe { str::from_utf8_unchecked(&self.vec) }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.vec.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.vec.is_empty()
    }

    pub fn clear(&mut self) {
        self.vec.clear();
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.vec
    }

    pub fn from_utf8(vec: Vec<u8>) -> Result<Self, Vec<u8>> {
        match core::str::from_utf8(&vec) {
            Ok(_) => Ok(String { vec }),
            Err(_) => Err(vec),
        }
    }

    pub fn push(&mut self, ch: char) {
        let mut buf = [0; 4];
        let bytes = ch.encode_utf8(&mut buf).as_bytes();
        self.vec.extend_from_slice(bytes);
    }

    pub fn from_utf16_lossy(utf16: &[u16]) -> Self {
        let mut bytes = Vec::new();
        for &code in utf16 {
            if code == 0 {
                break;
            }
            if code <= 0x7F {
                bytes.push(code as u8);
            } else if code <= 0x7FF {
                bytes.push(0xC0 | ((code >> 6) as u8));
                bytes.push(0x80 | (code as u8 & 0x3F));
            } else {
                bytes.push(0xE0 | ((code >> 12) as u8));
                bytes.push(0x80 | ((code >> 6) as u8 & 0x3F));
                bytes.push(0x80 | (code as u8 & 0x3F));
            }
        }
        String { vec: bytes }
    }

    /// Converts a byte slice to a `String` lossily, replacing invalid UTF‑8 with '�'.
    pub fn from_utf8_lossy(bytes: &[u8]) -> String {
        let mut vec = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match core::str::from_utf8(&bytes[i..]) {
                Ok(valid) => {
                    vec.extend_from_slice(valid.as_bytes());
                    break;
                }
                Err(e) => {
                    let valid_len = e.valid_up_to();
                    if valid_len > 0 {
                        vec.extend_from_slice(&bytes[i..i + valid_len]);
                    }
                    if let Some(error_len) = e.error_len() {
                        // replace with '�' (U+FFFD) which is 0xEF 0xBF 0xBD in UTF‑8
                        vec.extend_from_slice(&[0xEF, 0xBF, 0xBD]);
                        i += valid_len + error_len;
                    } else {
                        break;
                    }
                }
            }
        }
        String { vec }
    }
}

// No manual `impl Default` – it's derived.

impl From<&str> for String {
    fn from(s: &str) -> Self {
        Self::from(s)
    }
}

impl Deref for String {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl DerefMut for String {
    #[inline]
    fn deref_mut(&mut self) -> &mut str {
        unsafe { str::from_utf8_unchecked_mut(&mut self.vec) }
    }
}

impl From<&mut str> for String {
    fn from(s: &mut str) -> Self {
        Self::from(&*s)
    }
}

impl fmt::Display for String {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::String;

    #[test]
    fn new_and_push() {
        let mut s = String::new();
        s.push_str("Hello");
        s.push_str(", ");
        s.push_str("world!");
        assert_eq!(s.as_str(), "Hello, world!");
        assert_eq!(s.len(), 13);
    }

    #[test]
    fn from_str() {
        let s = String::from("test");
        assert_eq!(s.as_str(), "test");
        assert!(!s.is_empty());
    }

    #[test]
    fn clear() {
        let mut s = String::from("foo");
        assert_eq!(s.len(), 3);
        s.clear();
        assert_eq!(s.len(), 0);
        assert!(s.is_empty());
        s.push_str("bar");
        assert_eq!(s.as_str(), "bar");
    }

    #[test]
    fn deref() {
        let s = String::from("hello");
        assert_eq!(s.len(), 5); // via Deref to str
        assert_eq!(&s[1..4], "ell"); // slice indexing
    }
}
