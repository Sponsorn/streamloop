//! The stream key at rest: Windows DPAPI, current-user scope, base64 in config.json.
//! A blob only decrypts for the same Windows user on the same PC.

pub fn protect(plain: &str) -> Result<String, String> {
    dpapi(plain.as_bytes(), true).map(|b| b64_encode(&b))
}

pub fn unprotect(blob_b64: &str) -> Result<String, String> {
    let blob = b64_decode(blob_b64.trim())?;
    String::from_utf8(dpapi(&blob, false)?).map_err(|_| "DPAPI: decrypted key is not UTF-8".into())
}

#[cfg(windows)]
fn dpapi(data: &[u8], encrypt: bool) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
    const ENTROPY: &[u8] = b"streamloop-stream-key";
    let blob = |b: &[u8]| CRYPT_INTEGER_BLOB { cbData: b.len() as u32, pbData: b.as_ptr() as *mut u8 };
    let (input, entropy) = (blob(data), blob(ENTROPY));
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // SAFETY: the input blobs borrow live slices; DPAPI allocates `out`, freed below with LocalFree.
    unsafe {
        let ok = if encrypt {
            CryptProtectData(&input, std::ptr::null(), &entropy, std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        } else {
            CryptUnprotectData(&input, std::ptr::null_mut(), &entropy, std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        if ok == 0 {
            let what = if encrypt { "CryptProtectData" } else { "CryptUnprotectData (made on another PC or by another user, or damaged)" };
            return Err(format!("DPAPI: {what} failed: {}", std::io::Error::from_raw_os_error(GetLastError() as i32)));
        }
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        Ok(bytes)
    }
}

#[cfg(not(windows))]
fn dpapi(_data: &[u8], _encrypt: bool) -> Result<Vec<u8>, String> {
    Err("DPAPI is Windows-only".into())
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard alphabet, padded.
fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

fn b64_decode(text: &str) -> Result<Vec<u8>, String> {
    let bad = || "not valid base64".to_string();
    let t = text.as_bytes();
    if t.len() % 4 != 0 {
        return Err(bad());
    }
    let mut out = Vec::with_capacity(t.len() / 4 * 3);
    for (k, quad) in t.chunks(4).enumerate() {
        let pad = quad.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && k + 1 != t.len() / 4) {
            return Err(bad());
        }
        let mut n = 0u32;
        for (i, &c) in quad[..4 - pad].iter().enumerate() {
            let v = ALPHABET.iter().position(|&a| a == c).ok_or_else(bad)? as u32;
            n |= v << (18 - 6 * i);
        }
        out.extend_from_slice(&n.to_be_bytes()[1..4 - pad]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors_and_round_trips() {
        for (plain, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(b64_encode(plain.as_bytes()), enc);
            assert_eq!(b64_decode(enc).unwrap(), plain.as_bytes());
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(b64_decode(&b64_encode(&all)).unwrap(), all);
    }

    #[test]
    fn base64_rejects_bad_input() {
        for bad in ["Zg=", "Z", "Zm9v!", "Zg==Zg==", "=Zg="] {
            assert!(b64_decode(bad).is_err(), "{bad}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn protect_then_unprotect_round_trips_a_non_ascii_key() {
        let key = "live_123_äöü_ключ_🔑";
        let blob = protect(key).unwrap();
        assert!(!blob.contains(key));
        assert_eq!(unprotect(&blob).unwrap(), key);
    }

    #[cfg(windows)]
    #[test]
    fn unprotect_rejects_a_corrupted_blob() {
        let mut raw = b64_decode(&protect("live_abc").unwrap()).unwrap();
        let mid = raw.len() / 2;
        raw[mid] ^= 0xff;
        assert!(unprotect(&b64_encode(&raw)).is_err());
    }

    #[test]
    fn unprotect_rejects_invalid_base64() {
        let e = unprotect("not base64!").unwrap_err();
        assert!(e.contains("base64"), "{e}");
    }
}
