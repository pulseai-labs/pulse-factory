//! The pairing code (ADR-0012, `docs/contracts/api.md`).
//!
//! One copyable string: `pf1-` and then RFC 4648 base32, lower case, no padding, of
//! `version (1 byte, 1) ‖ secret (10 bytes) ‖ SPKI SHA-256 (32 bytes) ‖ the listen address as UTF-8`.
//! The secret has 80 bits and works once. The hash pins the server key. The address tells the
//! client where to connect.

/// The text every code starts with. The digit is the format version.
pub const CODE_PREFIX: &str = "pf1-";

/// The version byte inside the code.
pub const CODE_VERSION: u8 = 1;

/// The secret is 80 bits.
pub const SECRET_BYTES: usize = 10;

/// A SHA-256 is 32 bytes.
pub const SPKI_BYTES: usize = 32;

/// The longest listen address a code carries: `[ipv6]:port` fits in far less.
const LISTEN_MAX_BYTES: usize = 64;

const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// The parts of a pairing code.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingCode {
    pub secret: [u8; SECRET_BYTES],
    pub spki_sha256: [u8; SPKI_BYTES],
    /// The API address, such as `100.90.203.21:7443`.
    pub listen: String,
}

// The secret never shows in a debug print, so a stray `{:?}` cannot put it in a log.
impl std::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingCode")
            .field("secret", &"<hidden>")
            .field("spki_sha256", &hex(&self.spki_sha256))
            .field("listen", &self.listen)
            .finish()
    }
}

/// Why a text is not a pairing code. The text never says more than this: a refusal over the wire
/// is one answer for every reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeError {
    Prefix,
    Alphabet,
    Length,
    Version,
    Address,
}

impl std::fmt::Display for CodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CodeError::Prefix => "the code does not start with pf1-",
            CodeError::Alphabet => "the code holds a character that base32 does not use",
            CodeError::Length => "the code has the wrong length",
            CodeError::Version => "the code has an unknown version",
            CodeError::Address => "the code holds no address",
        })
    }
}

impl std::error::Error for CodeError {}

impl PairingCode {
    /// The code as one string.
    pub fn encode(&self) -> String {
        let mut bytes = Vec::with_capacity(1 + SECRET_BYTES + SPKI_BYTES + self.listen.len());
        bytes.push(CODE_VERSION);
        bytes.extend_from_slice(&self.secret);
        bytes.extend_from_slice(&self.spki_sha256);
        bytes.extend_from_slice(self.listen.as_bytes());
        format!("{CODE_PREFIX}{}", base32_encode(&bytes))
    }

    /// Read a code. Surrounding white space is ignored, and so is the case of the letters: a code
    /// that was copied through a system that changed it still reads.
    pub fn decode(text: &str) -> Result<PairingCode, CodeError> {
        let text = text.trim();
        let rest = text
            .get(..CODE_PREFIX.len())
            .filter(|p| p.eq_ignore_ascii_case(CODE_PREFIX))
            .map(|_| &text[CODE_PREFIX.len()..])
            .ok_or(CodeError::Prefix)?;
        let bytes = base32_decode(rest)?;
        let head = 1 + SECRET_BYTES + SPKI_BYTES;
        if bytes.len() <= head || bytes.len() > head + LISTEN_MAX_BYTES {
            return Err(CodeError::Length);
        }
        if bytes[0] != CODE_VERSION {
            return Err(CodeError::Version);
        }
        let mut secret = [0u8; SECRET_BYTES];
        secret.copy_from_slice(&bytes[1..=SECRET_BYTES]);
        let mut spki = [0u8; SPKI_BYTES];
        spki.copy_from_slice(&bytes[1 + SECRET_BYTES..head]);
        let listen = String::from_utf8(bytes[head..].to_vec()).map_err(|_| CodeError::Address)?;
        if listen.is_empty() || listen.chars().any(char::is_control) {
            return Err(CodeError::Address);
        }
        Ok(PairingCode {
            secret,
            spki_sha256: spki,
            listen,
        })
    }

    /// The secret as 20 lower-case hex digits: the form the host port takes.
    pub fn secret_hex(&self) -> String {
        hex(&self.secret)
    }
}

fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((acc >> bits) & 31) as usize]));
        }
        acc &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((acc << (5 - bits)) & 31) as usize]));
    }
    out
}

fn base32_decode(text: &str) -> Result<Vec<u8>, CodeError> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in text.bytes() {
        let value = match c.to_ascii_lowercase() {
            c @ b'a'..=b'z' => c - b'a',
            c @ b'2'..=b'7' => c - b'2' + 26,
            _ => return Err(CodeError::Alphabet),
        };
        acc = (acc << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).unwrap_or(0));
            acc &= (1 << bits) - 1;
        }
    }
    // The bits that are left pad the last character. Padding is zero in a code this crate wrote.
    if acc != 0 {
        return Err(CodeError::Alphabet);
    }
    Ok(out)
}

/// Lower-case hex of some bytes.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The bytes of a hex text of even length, or `None`.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// The SHA-256 of a device token as the store keeps it: 64 hex digits of the hash over the 32 bytes
/// that the 64 hex digits of the token stand for. `None` when the text is not a token.
pub fn token_sha256(token: &str) -> Option<String> {
    if token.len() != 64 {
        return None;
    }
    let bytes = unhex(token)?;
    Some(pf_engine::claim::sha256_hex(&bytes))
}
