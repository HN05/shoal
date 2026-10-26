//! The media type Forgejo detects in an attached file, which the media types
//! of its attachment settings match: Go's `http.DetectContentType`, without
//! its parameters.

/// How many leading bytes Go reads.
pub(super) const SNIFF_LEN: usize = 512;

/// A leading byte pattern and the media type it means, in Go's order.
enum Signature {
    /// An HTML tag in any case after leading whitespace, followed by a space
    /// or `>`.
    Html(&'static [u8]),
    /// Bytes that equal `pattern` where `mask` keeps their bits.
    Masked {
        mask: &'static [u8],
        pattern: &'static [u8],
        skip_whitespace: bool,
        media: &'static str,
    },
    Prefix(&'static [u8], &'static str),
    /// An ISO media file whose `ftyp` box names an `mp4` brand.
    Mp4,
}

use Signature::{Html, Masked, Mp4, Prefix};

const SIGNATURES: &[Signature] = &[
    Html(b"<!DOCTYPE HTML"),
    Html(b"<HTML"),
    Html(b"<HEAD"),
    Html(b"<SCRIPT"),
    Html(b"<IFRAME"),
    Html(b"<H1"),
    Html(b"<DIV"),
    Html(b"<FONT"),
    Html(b"<TABLE"),
    Html(b"<A"),
    Html(b"<STYLE"),
    Html(b"<TITLE"),
    Html(b"<B"),
    Html(b"<BODY"),
    Html(b"<BR"),
    Html(b"<P"),
    Html(b"<!--"),
    Masked {
        mask: b"\xFF\xFF\xFF\xFF\xFF",
        pattern: b"<?xml",
        skip_whitespace: true,
        media: "text/xml",
    },
    Prefix(b"%PDF-", "application/pdf"),
    Prefix(b"%!PS-Adobe-", "application/postscript"),
    Masked {
        mask: b"\xFF\xFF\x00\x00",
        pattern: b"\xFE\xFF\x00\x00",
        skip_whitespace: false,
        media: "text/plain",
    },
    Masked {
        mask: b"\xFF\xFF\x00\x00",
        pattern: b"\xFF\xFE\x00\x00",
        skip_whitespace: false,
        media: "text/plain",
    },
    Masked {
        mask: b"\xFF\xFF\xFF\x00",
        pattern: b"\xEF\xBB\xBF\x00",
        skip_whitespace: false,
        media: "text/plain",
    },
    Prefix(b"\x00\x00\x01\x00", "image/x-icon"),
    Prefix(b"\x00\x00\x02\x00", "image/x-icon"),
    Prefix(b"BM", "image/bmp"),
    Prefix(b"GIF87a", "image/gif"),
    Prefix(b"GIF89a", "image/gif"),
    Masked {
        mask: b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF\xFF\xFF",
        pattern: b"RIFF\x00\x00\x00\x00WEBPVP",
        skip_whitespace: false,
        media: "image/webp",
    },
    Prefix(b"\x89PNG\x0D\x0A\x1A\x0A", "image/png"),
    Prefix(b"\xFF\xD8\xFF", "image/jpeg"),
    Masked {
        mask: b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF",
        pattern: b"FORM\x00\x00\x00\x00AIFF",
        skip_whitespace: false,
        media: "audio/aiff",
    },
    Prefix(b"ID3", "audio/mpeg"),
    Prefix(b"OggS\x00", "application/ogg"),
    Prefix(b"MThd\x00\x00\x00\x06", "audio/midi"),
    Masked {
        mask: b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF",
        pattern: b"RIFF\x00\x00\x00\x00AVI ",
        skip_whitespace: false,
        media: "video/avi",
    },
    Masked {
        mask: b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF",
        pattern: b"RIFF\x00\x00\x00\x00WAVE",
        skip_whitespace: false,
        media: "audio/wave",
    },
    Mp4,
    Prefix(b"\x1A\x45\xDF\xA3", "video/webm"),
    Masked {
        mask: b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xFF\xFF",
        pattern: b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00LP",
        skip_whitespace: false,
        media: "application/vnd.ms-fontobject",
    },
    Prefix(b"\x00\x01\x00\x00", "font/ttf"),
    Prefix(b"OTTO", "font/otf"),
    Prefix(b"ttcf", "font/collection"),
    Prefix(b"wOFF", "font/woff"),
    Prefix(b"wOF2", "font/woff2"),
    Prefix(b"\x1F\x8B\x08", "application/x-gzip"),
    Prefix(b"PK\x03\x04", "application/zip"),
    Prefix(b"Rar!\x1A\x07\x00", "application/x-rar-compressed"),
    Prefix(b"Rar!\x1A\x07\x01\x00", "application/x-rar-compressed"),
    Prefix(b"\x00\x61\x73\x6D", "application/wasm"),
];

/// The media type of a file that starts with `data`.
pub(super) fn media_type(data: &[u8]) -> &'static str {
    let data = &data[..data.len().min(SNIFF_LEN)];
    let text = data
        .iter()
        .position(|&byte| !is_whitespace(byte))
        .map_or(&data[data.len()..], |start| &data[start..]);
    SIGNATURES
        .iter()
        .find_map(|signature| signature.matches(data, text))
        .or_else(|| {
            text.iter()
                .all(|&byte| !is_binary(byte))
                .then_some("text/plain")
        })
        .unwrap_or("application/octet-stream")
}

impl Signature {
    /// The media type if `data`, which is `text` after leading whitespace,
    /// matches.
    fn matches(&self, data: &[u8], text: &[u8]) -> Option<&'static str> {
        match *self {
            Html(tag) => {
                let next = text.get(tag.len())?;
                let same = tag.iter().zip(text).all(|(&expected, &byte)| {
                    let byte = if expected.is_ascii_uppercase() {
                        byte & 0xDF
                    } else {
                        byte
                    };
                    expected == byte
                });
                (same && matches!(next, b' ' | b'>')).then_some("text/html")
            }
            Masked {
                mask,
                pattern,
                skip_whitespace,
                media,
            } => {
                let data = if skip_whitespace { text } else { data };
                let same = data.len() >= pattern.len()
                    && pattern
                        .iter()
                        .zip(mask)
                        .zip(data)
                        .all(|((&expected, &mask), &byte)| byte & mask == expected);
                same.then_some(media)
            }
            Prefix(prefix, media) => data.starts_with(prefix).then_some(media),
            Mp4 => is_mp4(data).then_some("video/mp4"),
        }
    }
}

fn is_mp4(data: &[u8]) -> bool {
    let Some(size) = data.get(..4) else {
        return false;
    };
    let size = u32::from_be_bytes([size[0], size[1], size[2], size[3]]) as usize;
    if data.len() < 12 || data.len() < size || !size.is_multiple_of(4) || &data[4..8] != b"ftyp" {
        return false;
    }
    // The brands after the major one at 8, skipping the minor version at 12.
    (8..size)
        .step_by(4)
        .filter(|&start| start != 12)
        .any(|start| data[start..].starts_with(b"mp4"))
}

fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

/// A control byte plain text does not contain.
fn is_binary(byte: u8) -> bool {
    matches!(byte, 0x00..=0x08 | 0x0B | 0x0E..=0x1A | 0x1C..=0x1F)
}

#[cfg(test)]
mod tests;
