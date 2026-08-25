//! Reading the JPEG a camera already wrote inside a raw file.
//!
//! FFmpeg is not a raw converter. It has no demuxer for Fujifilm RAF at all —
//! `Invalid data found when processing input`, which is how a 92-file shoot
//! ends up with no thumbnails and no faces — and where it does open a raw, as
//! it does for DNG, it hands back undemosaiced sensor data: a `bayer_rggb16le`
//! plane with no white balance and no tone curve, which lands rotated, dark and
//! colour-shifted next to the camera's own JPEG of the same frame.
//!
//! Every raw format embeds a finished JPEG preview, because the camera needs
//! something to put on its own screen. That preview is the picture the
//! photographer saw: correct colour, correct orientation, and on the GFX100 II
//! files this was written against 4000x3000 — several times more than a
//! detector working at 1024 will ever use. Reading it is a seek and a slice
//! rather than a demosaic of a hundred megapixels.
//!
//! So this module looks for that preview three ways, cheapest first:
//!
//! 1. **RAF** stores the offset and length in its own header, at bytes 84 and
//!    88. No parsing, just two big-endian words.
//! 2. **TIFF-based raws** (DNG, ARW, NEF, CR2, and the near-TIFF ORF and RW2)
//!    keep previews in their IFDs, either as the JPEG tag pair or as a
//!    single-strip JPEG image in a sub-IFD.
//! 3. Anything else — the ISO-BMFF boxes of a CR3, or a variant nobody here has
//!    a sample of — gets a bounded scan for the largest embedded JPEG.
//!
//! Every candidate is *validated* by decoding its header, and the largest valid
//! one wins. That matters: a raw's main image is often itself JPEG-compressed
//! (lossless JPEG, in CR2 and NEF), and those bytes look like a preview to a
//! scanner without being a picture. Validation rejects them.

use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use image::{ImageFormat, ImageReader};

use crate::formats;

/// Extensions worth looking inside.
///
/// HEIC, HEIF and AVIF are deliberately absent: they are finished pictures that
/// FFmpeg decodes properly, not a container wrapped around sensor data.
const RAW_EXTENSIONS: &[&str] = &["raf", "dng", "arw", "nef", "cr2", "cr3", "orf", "rw2", "srw", "pef"];

/// Long edge at or above which a preview is the picture rather than a
/// contact-sheet thumbnail. Below it FFmpeg gets its turn first: a 160x120
/// thumbnail is worse than a demosaic, while a 1024px preview is better.
pub const USEFUL_LONG_EDGE: u32 = 1024;

/// Anything smaller is not a picture at all, whatever the tags claim.
const MIN_EDGE: u32 = 64;

/// How far into a file the blind scan looks. Previews live near the front — RAF
/// puts one at byte 148 — and this caps what an unrecognised 200 MB file costs.
const SCAN_WINDOW: usize = 16 * 1024 * 1024;

/// Ceilings for the IFD walk. A malformed file must not be able to make this
/// loop for ever, and no real raw needs more than a handful.
const MAX_IFD_VISITS: usize = 16;
const MAX_IFD_DEPTH: u32 = 2;
const MAX_CANDIDATES: usize = 12;

/// How much of a preview is enough to read its EXIF and its dimensions.
///
/// Both live in the segments before the first scan, so a metadata pass has no
/// reason to pull a 12 MB preview off a NAS in full — which is what a
/// five-hundred-file DNG shoot would otherwise do twice, once to index and once
/// to decode.
const METADATA_HEAD: usize = 256 * 1024;

/// The largest JPEG found inside a raw file, already validated.
#[derive(Clone)]
pub struct Preview {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// False when `bytes` holds only the head of the preview, which is enough
    /// to read dimensions and EXIF from but not to decode.
    pub complete: bool,
}

impl Preview {
    pub fn long_edge(&self) -> u32 {
        self.width.max(self.height)
    }

    /// True when this is worth using in place of a demosaic.
    pub fn is_useful(&self) -> bool {
        self.long_edge() >= USEFUL_LONG_EDGE
    }
}

impl std::fmt::Debug for Preview {
    /// Spelling out the byte count rather than the bytes: a megabyte of JPEG in
    /// a log line is not a diagnostic.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preview")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Whether this path is a raw file worth looking inside.
pub fn is_raw(path: &Path) -> bool {
    RAW_EXTENSIONS.contains(&formats::extension(path).as_str())
}

/// The best embedded preview in `path`, or `None` if it holds no usable JPEG.
///
/// Never fails loudly: a raw whose preview cannot be found is a raw the caller
/// decodes some other way, not an error to report.
pub fn best_preview(path: &Path) -> Option<Preview> {
    read_preview(path, None)
}

/// Enough of the best preview to read its dimensions and EXIF.
///
/// The returned `bytes` are truncated and `complete` is false: use this to
/// index a file, not to decode it.
pub fn preview_metadata(path: &Path) -> Option<Preview> {
    read_preview(path, Some(METADATA_HEAD))
}

fn read_preview(path: &Path, cap: Option<usize>) -> Option<Preview> {
    let mut file = File::open(path).ok()?;

    let mut head = [0u8; 64];
    let read = read_at(&mut file, 0, &mut head)?;
    let head = &head[..read];

    let mut offsets: Vec<(u64, usize)> = Vec::new();

    if let Some(pair) = raf_offset(&mut file, head) {
        offsets.push(pair);
    } else if let Some(endian) = tiff_endian(head) {
        let first_ifd = endian.u32(head.get(4..8)?);
        let mut visits = 0;
        tiff_candidates(&mut file, endian, first_ifd, 0, &mut visits, &mut offsets);
    }

    let mut best: Option<Preview> = None;
    for (offset, length) in offsets.iter().take(MAX_CANDIDATES) {
        if let Some(preview) = read_and_validate(&mut file, *offset, *length, cap) {
            best = keep_larger(best, preview);
        }
    }

    // Only when the structured routes found nothing: a blind scan reads far
    // more than a seek to a known offset.
    if best.is_none() {
        for (offset, length) in scan_candidates(&mut file).iter().take(MAX_CANDIDATES) {
            if let Some(preview) = read_and_validate(&mut file, *offset, *length, cap) {
                best = keep_larger(best, preview);
            }
        }
    }

    best
}

fn keep_larger(current: Option<Preview>, candidate: Preview) -> Option<Preview> {
    match current {
        Some(existing) if pixels(&existing) >= pixels(&candidate) => Some(existing),
        _ => Some(candidate),
    }
}

fn pixels(preview: &Preview) -> u64 {
    u64::from(preview.width) * u64::from(preview.height)
}

/// Fujifilm's header carries the preview's position directly: a 16-byte magic,
/// then camera identification, then at byte 84 the JPEG's offset and at 88 its
/// length, both big-endian regardless of anything else in the file.
fn raf_offset(file: &mut File, head: &[u8]) -> Option<(u64, usize)> {
    if !head.starts_with(b"FUJIFILMCCD-RAW") {
        return None;
    }
    let mut words = [0u8; 8];
    read_at(file, 84, &mut words)?;
    let offset = u32::from_be_bytes([words[0], words[1], words[2], words[3]]);
    let length = u32::from_be_bytes([words[4], words[5], words[6], words[7]]);
    if offset == 0 || length == 0 {
        return None;
    }
    Some((u64::from(offset), length as usize))
}

#[derive(Clone, Copy)]
struct Endian {
    little: bool,
}

impl Endian {
    fn u16(&self, bytes: &[u8]) -> u16 {
        let pair = [bytes[0], bytes[1]];
        if self.little {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }
    }

    fn u32(&self, bytes: &[u8]) -> u32 {
        let word = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if self.little {
            u32::from_le_bytes(word)
        } else {
            u32::from_be_bytes(word)
        }
    }
}

/// Byte order for a TIFF-shaped file.
///
/// The magic word after the byte order is deliberately not checked against 42:
/// Olympus writes `IIRO` and Panasonic `IIU\0` where TIFF puts `II*\0`, and both
/// lay their IFDs out the same way. Reading the offset and finding no valid IFD
/// costs one seek; refusing to look costs the format.
fn tiff_endian(head: &[u8]) -> Option<Endian> {
    match head.get(..2)? {
        b"II" => Some(Endian { little: true }),
        b"MM" => Some(Endian { little: false }),
        _ => None,
    }
}

// TIFF tags this walk cares about.
const TAG_COMPRESSION: u16 = 0x0103;
const TAG_STRIP_OFFSETS: u16 = 0x0111;
const TAG_STRIP_BYTE_COUNTS: u16 = 0x0117;
const TAG_JPEG_OFFSET: u16 = 0x0201;
const TAG_JPEG_LENGTH: u16 = 0x0202;
const TAG_SUB_IFDS: u16 = 0x014A;

/// JPEG compression, old-style and current. A strip carrying one of these is a
/// JPEG image rather than a plane of samples.
const COMPRESSION_JPEG_OLD: u32 = 6;
const COMPRESSION_JPEG: u32 = 7;

/// Walks one IFD, its sub-IFDs and its successors, collecting anything that
/// claims to be a JPEG.
fn tiff_candidates(
    file: &mut File,
    endian: Endian,
    ifd_offset: u32,
    depth: u32,
    visits: &mut usize,
    out: &mut Vec<(u64, usize)>,
) {
    if ifd_offset == 0 || depth > MAX_IFD_DEPTH || *visits >= MAX_IFD_VISITS || out.len() >= MAX_CANDIDATES {
        return;
    }
    *visits += 1;

    let mut count_bytes = [0u8; 2];
    if read_at(file, u64::from(ifd_offset), &mut count_bytes).is_none() {
        return;
    }
    let entries = endian.u16(&count_bytes);
    // Twelve bytes an entry, and an IFD with thousands of them is not a raw.
    if entries == 0 || entries > 512 {
        return;
    }

    let mut table = vec![0u8; usize::from(entries) * 12 + 4];
    let read = match read_at(file, u64::from(ifd_offset) + 2, &mut table) {
        Some(read) => read,
        None => return,
    };
    table.truncate(read);

    let mut jpeg_offset = None;
    let mut jpeg_length = None;
    let mut strip_offset = None;
    let mut strip_length = None;
    let mut compression = None;
    let mut sub_ifds: Vec<u32> = Vec::new();

    for entry in table.chunks_exact(12).take(usize::from(entries)) {
        let tag = endian.u16(&entry[0..2]);
        let kind = endian.u16(&entry[2..4]);
        let count = endian.u32(&entry[4..8]);
        let value = &entry[8..12];

        match tag {
            TAG_JPEG_OFFSET => jpeg_offset = scalar(endian, kind, count, value),
            TAG_JPEG_LENGTH => jpeg_length = scalar(endian, kind, count, value),
            TAG_STRIP_OFFSETS => strip_offset = scalar(endian, kind, count, value),
            TAG_STRIP_BYTE_COUNTS => strip_length = scalar(endian, kind, count, value),
            TAG_COMPRESSION => compression = scalar(endian, kind, count, value),
            TAG_SUB_IFDS => sub_ifds = long_array(file, endian, kind, count, value),
            _ => {}
        }
    }

    if let (Some(offset), Some(length)) = (jpeg_offset, jpeg_length) {
        push_candidate(out, offset, length);
    }

    // A single-strip image compressed as JPEG is a preview — or, in a CR2 or
    // NEF, the lossless-JPEG main image, which validation throws out.
    if matches!(compression, Some(COMPRESSION_JPEG) | Some(COMPRESSION_JPEG_OLD)) {
        if let (Some(offset), Some(length)) = (strip_offset, strip_length) {
            push_candidate(out, offset, length);
        }
    }

    for sub in sub_ifds.into_iter().take(8) {
        tiff_candidates(file, endian, sub, depth + 1, visits, out);
    }

    let next = table
        .get(usize::from(entries) * 12..usize::from(entries) * 12 + 4)
        .map(|bytes| endian.u32(bytes))
        .unwrap_or(0);
    // A file pointing an IFD at itself would otherwise spin until the visit
    // ceiling, which is a slow way to say malformed.
    if next != ifd_offset {
        tiff_candidates(file, endian, next, depth, visits, out);
    }
}

fn push_candidate(out: &mut Vec<(u64, usize)>, offset: u32, length: u32) {
    if offset == 0 || length == 0 || out.len() >= MAX_CANDIDATES {
        return;
    }
    out.push((u64::from(offset), length as usize));
}

/// One SHORT or LONG, which TIFF stores inline when it fits in the four value
/// bytes — and a single one always does.
fn scalar(endian: Endian, kind: u16, count: u32, value: &[u8]) -> Option<u32> {
    if count != 1 {
        return None;
    }
    match kind {
        3 => Some(u32::from(endian.u16(&value[0..2]))),
        4 => Some(endian.u32(value)),
        _ => None,
    }
}

/// An array of LONGs, inline when it is one element and out of line otherwise.
fn long_array(file: &mut File, endian: Endian, kind: u16, count: u32, value: &[u8]) -> Vec<u32> {
    if kind != 4 || count == 0 || count > 8 {
        return Vec::new();
    }
    if count == 1 {
        return vec![endian.u32(value)];
    }
    let mut buffer = vec![0u8; count as usize * 4];
    let at = u64::from(endian.u32(value));
    match read_at(file, at, &mut buffer) {
        Some(read) if read == buffer.len() => buffer.chunks_exact(4).map(|word| endian.u32(word)).collect(),
        _ => Vec::new(),
    }
}

/// Every JPEG start marker in the first window, paired with the end of the
/// image that starts there. The fallback for a container this cannot parse.
fn scan_candidates(file: &mut File) -> Vec<(u64, usize)> {
    let mut window = vec![0u8; SCAN_WINDOW];
    let read = match read_at(file, 0, &mut window) {
        Some(read) => read,
        None => return Vec::new(),
    };
    let window = &window[..read];

    let mut found = Vec::new();
    let mut at = 0usize;
    while at + 3 < window.len() && found.len() < MAX_CANDIDATES {
        if &window[at..at + 3] == b"\xff\xd8\xff" {
            if let Some(length) = jpeg_length(&window[at..]) {
                found.push((at as u64, length));
                at += length;
                continue;
            }
        }
        at += 1;
    }
    found
}

/// Length of the JPEG starting at the front of `bytes`, by walking its markers.
///
/// Searching for the end-of-image marker instead does not work, in either
/// direction, and both failures are quiet:
///
/// - the *last* one belongs to whatever JPEG comes last in the file, so a
///   thumbnail swallows the full-size preview stored after it, and the
///   dimensions read back are the thumbnail's;
/// - the *first* one often belongs to the small thumbnail a camera puts inside
///   this JPEG's own EXIF block, which truncates the file before its start-of-
///   frame and leaves nothing decodable.
///
/// Walking the markers skips each segment by its declared length, so an
/// embedded thumbnail is stepped over rather than mistaken for the end.
fn jpeg_length(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }

    let mut at = 2usize;
    loop {
        // Markers may be preceded by any number of 0xFF fill bytes.
        let mut marker_at = at;
        while marker_at < bytes.len() && bytes[marker_at] == 0xFF {
            marker_at += 1;
        }
        if marker_at >= bytes.len() || marker_at == at {
            return None; // Ran out, or landed somewhere that is not a marker.
        }
        let marker = bytes[marker_at];
        at = marker_at + 1;

        match marker {
            // End of image: the length is everything up to and including it.
            0xD9 => return Some(at),
            // Markers that carry no payload.
            0xD8 | 0x01 | 0xD0..=0xD7 => {}
            // Start of scan: a segment, then entropy-coded data that runs until
            // the next real marker.
            0xDA => {
                at += segment_length(bytes, at)?;
                while at + 1 < bytes.len() {
                    if bytes[at] == 0xFF {
                        let next = bytes[at + 1];
                        // 0x00 is a stuffed 0xFF and RST markers punctuate the
                        // scan; neither ends it.
                        if next == 0x00 || (0xD0..=0xD7).contains(&next) {
                            at += 2;
                            continue;
                        }
                        break;
                    }
                    at += 1;
                }
                if at + 1 >= bytes.len() {
                    return None;
                }
            }
            _ => at += segment_length(bytes, at)?,
        }
    }
}

/// The declared length of the segment whose two length bytes start at `at`,
/// which includes those two bytes.
fn segment_length(bytes: &[u8], at: usize) -> Option<usize> {
    let pair = bytes.get(at..at + 2)?;
    let length = usize::from(u16::from_be_bytes([pair[0], pair[1]]));
    if length < 2 || at + length > bytes.len() {
        return None;
    }
    Some(length)
}

/// Reads the candidate and confirms it really is a decodable JPEG.
///
/// `cap` limits how much is read: enough for the header when only the
/// dimensions and EXIF are wanted, all of it when the picture is.
fn read_and_validate(file: &mut File, offset: u64, length: usize, cap: Option<usize>) -> Option<Preview> {
    // A preview larger than this is not a preview; refusing to allocate keeps a
    // corrupt length field from asking for a gigabyte.
    if !(64..=128 * 1024 * 1024).contains(&length) {
        return None;
    }

    let wanted = cap.map_or(length, |cap| length.min(cap));
    let complete = wanted == length;

    let mut bytes = vec![0u8; wanted];
    let read = read_at(file, offset, &mut bytes)?;
    let complete = complete && read == length;
    bytes.truncate(read);

    // Some cameras record a length that runs past the end of the image, and
    // decoders differ on whether they mind — so cut at the marker when it is
    // there.
    if let Some(length) = jpeg_length(&bytes) {
        bytes.truncate(length);
    }

    let reader = ImageReader::new(Cursor::new(&bytes)).with_guessed_format().ok()?;
    // Only JPEG: the point is to reuse the camera's own rendering, and a stray
    // TIFF plane found by the scanner is not that.
    if reader.format() != Some(ImageFormat::Jpeg) {
        return None;
    }
    let (width, height) = reader.into_dimensions().ok()?;
    if width < MIN_EDGE || height < MIN_EDGE {
        return None;
    }

    Some(Preview {
        bytes,
        width,
        height,
        complete,
    })
}

/// Fills `buffer` from `offset`, returning how much was actually read.
///
/// A short read is normal at the end of a file and not an error; `None` means
/// the seek or the read itself failed.
fn read_at(file: &mut File, offset: u64, buffer: &mut [u8]) -> Option<usize> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    Some(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageEncoder, Rgb, RgbImage};
    use std::io::Write;

    /// A real JPEG of the given size, so validation has something to accept.
    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut image = RgbImage::new(width, height);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            *pixel = Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
        }
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 80)
            .write_image(image.as_raw(), width, height, image::ExtendedColorType::Rgb8)
            .unwrap();
        bytes
    }

    fn write_temp(name: &str, bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        File::create(&path).unwrap().write_all(bytes).unwrap();
        (dir, path)
    }

    /// The layout of a Fujifilm file: magic, then the offset and length of the
    /// preview as big-endian words at bytes 84 and 88.
    fn raf_container(preview: &[u8]) -> Vec<u8> {
        let offset: u32 = 148;
        let mut bytes = vec![0u8; offset as usize];
        bytes[..16].copy_from_slice(b"FUJIFILMCCD-RAW ");
        bytes[84..88].copy_from_slice(&offset.to_be_bytes());
        bytes[88..92].copy_from_slice(&(preview.len() as u32).to_be_bytes());
        bytes.extend_from_slice(preview);
        // Sensor data would follow; a tail of zeroes stands in for it.
        bytes.extend_from_slice(&[0u8; 4096]);
        bytes
    }

    #[test]
    fn finds_the_preview_in_a_fujifilm_header() {
        let preview = jpeg(1600, 1200);
        let (_dir, path) = write_temp("DSCF0001.RAF", &raf_container(&preview));

        let found = best_preview(&path).expect("a RAF header points straight at its preview");
        assert_eq!((found.width, found.height), (1600, 1200));
        assert!(found.is_useful());
        assert_eq!(found.bytes, preview);
    }

    /// A little-endian TIFF whose first IFD carries the JPEG tag pair — the
    /// shape DNG, ARW and NEF previews take.
    fn tiff_container(preview: &[u8], magic: [u8; 2]) -> Vec<u8> {
        let header_len = 8u32;
        let entries: u16 = 2;
        let ifd_len = 2 + u32::from(entries) * 12 + 4;
        let preview_at = header_len + ifd_len;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"II");
        bytes.extend_from_slice(&magic);
        bytes.extend_from_slice(&header_len.to_le_bytes());
        bytes.extend_from_slice(&entries.to_le_bytes());

        for (tag, value) in [(TAG_JPEG_OFFSET, preview_at), (TAG_JPEG_LENGTH, preview.len() as u32)] {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&4u16.to_le_bytes()); // LONG
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
        }

        bytes.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
        bytes.extend_from_slice(preview);
        bytes
    }

    #[test]
    fn finds_the_preview_in_a_tiff_ifd() {
        let preview = jpeg(2048, 1365);
        let (_dir, path) = write_temp("P1001349.DNG", &tiff_container(&preview, [42, 0]));

        let found = best_preview(&path).expect("the JPEG tag pair names the preview");
        assert_eq!((found.width, found.height), (2048, 1365));
        assert!(found.is_useful());
    }

    /// Olympus writes a magic word of `RO` and Panasonic `U\0` where TIFF puts
    /// `*\0`. Insisting on 42 would lose both formats for no gain.
    #[test]
    fn reads_a_near_tiff_magic_word() {
        let preview = jpeg(1280, 960);
        let (_dir, path) = write_temp("PA010203.ORF", &tiff_container(&preview, *b"RO"));

        let found = best_preview(&path).expect("ORF lays its IFDs out like TIFF");
        assert_eq!((found.width, found.height), (1280, 960));
    }

    #[test]
    fn falls_back_to_scanning_an_unparseable_container() {
        // Stands in for a CR3, whose ISO-BMFF boxes this module does not parse.
        let preview = jpeg(1920, 1080);
        let mut bytes = b"\x00\x00\x00\x18ftypcrx crx isom".to_vec();
        bytes.extend_from_slice(&[0u8; 512]);
        bytes.extend_from_slice(&preview);
        let (_dir, path) = write_temp("IMG_0001.CR3", &bytes);

        let found = best_preview(&path).expect("the scan finds an embedded JPEG anywhere");
        assert_eq!((found.width, found.height), (1920, 1080));
    }

    #[test]
    fn prefers_the_largest_preview_over_the_thumbnail() {
        // Cameras embed several. The contact-sheet thumbnail comes first in the
        // file, so taking the first match would take the worst one.
        let thumbnail = jpeg(160, 120);
        let preview = jpeg(2400, 1600);
        let mut bytes = b"\x00\x00\x00\x18ftypcrx crx isom".to_vec();
        bytes.extend_from_slice(&thumbnail);
        bytes.extend_from_slice(&[0u8; 64]);
        bytes.extend_from_slice(&preview);
        let (_dir, path) = write_temp("IMG_0002.CR3", &bytes);

        let found = best_preview(&path).unwrap();
        assert_eq!((found.width, found.height), (2400, 1600));
    }

    #[test]
    fn a_small_preview_is_found_but_not_called_useful() {
        // Worth having when FFmpeg cannot open the file at all, and worth
        // skipping when it can — which is the distinction is_useful draws.
        let (_dir, path) = write_temp("DSCF0002.RAF", &raf_container(&jpeg(160, 120)));

        let found = best_preview(&path).unwrap();
        assert_eq!((found.width, found.height), (160, 120));
        assert!(!found.is_useful());
    }

    #[test]
    fn a_file_with_no_embedded_jpeg_yields_nothing() {
        let (_dir, path) = write_temp("DSCF0003.RAF", &vec![7u8; 200_000]);
        assert!(best_preview(&path).is_none());
    }

    #[test]
    fn a_corrupt_length_is_refused_rather_than_allocated() {
        // The length word claims a gigabyte; the file is a few hundred bytes.
        let mut bytes = vec![0u8; 148];
        bytes[..16].copy_from_slice(b"FUJIFILMCCD-RAW ");
        bytes[84..88].copy_from_slice(&148u32.to_be_bytes());
        bytes[88..92].copy_from_slice(&1_000_000_000u32.to_be_bytes());
        bytes.extend_from_slice(&jpeg(320, 240));
        let (_dir, path) = write_temp("DSCF0004.RAF", &bytes);

        // A short read, truncated at the marker, is still a valid picture.
        let found = best_preview(&path).unwrap();
        assert_eq!((found.width, found.height), (320, 240));
    }

    #[test]
    fn an_ifd_pointing_at_itself_terminates() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"II");
        bytes.extend_from_slice(&42u16.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&TAG_COMPRESSION.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes()); // next IFD is this IFD
        let (_dir, path) = write_temp("loop.DNG", &bytes);

        assert!(best_preview(&path).is_none());
    }

    #[test]
    fn the_metadata_read_takes_the_head_and_says_so() {
        // A preview well past the cap, so the difference is visible.
        let preview = jpeg(3000, 2000);
        assert!(preview.len() > METADATA_HEAD, "test needs a preview past the cap");
        let (_dir, path) = write_temp("DSCF0005.RAF", &raf_container(&preview));

        let head = preview_metadata(&path).unwrap();
        // Dimensions still read, because they sit before the first scan.
        assert_eq!((head.width, head.height), (3000, 2000));
        assert!(!head.complete);
        assert_eq!(head.bytes.len(), METADATA_HEAD);

        let whole = best_preview(&path).unwrap();
        assert!(whole.complete);
        assert_eq!(whole.bytes, preview);
    }

    #[test]
    fn only_raw_extensions_are_worth_opening() {
        assert!(is_raw(Path::new("a/DSCF1075.RAF")));
        assert!(is_raw(Path::new("a/P1001349.dng")));
        assert!(is_raw(Path::new("a/shot.ARW")));
        // Finished pictures, decoded directly rather than unwrapped.
        assert!(!is_raw(Path::new("a/photo.jpg")));
        assert!(!is_raw(Path::new("a/photo.heic")));
        assert!(!is_raw(Path::new("a/clip.mp4")));
    }
}
