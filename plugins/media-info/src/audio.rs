//! Audio duration from a header prefix. WAV, FLAC, MP3.
//!
//! WAV and FLAC state their length in the header. MP3 does not: a `Xing` or
//! `Info` frame after the first header carries the frame count when the
//! encoder wrote one, and otherwise the first frame's bitrate over the file
//! size is the constant-bitrate estimate every player shows. `file_len` is
//! what `stat` said; the bytes are a prefix.

fn le32(b: &[u8], at: usize) -> Option<u64> {
    let s = b.get(at..at + 4)?;
    Some(u64::from(u32::from_le_bytes([s[0], s[1], s[2], s[3]])))
}

fn be32(b: &[u8], at: usize) -> Option<u64> {
    let s = b.get(at..at + 4)?;
    Some(u64::from(u32::from_be_bytes([s[0], s[1], s[2], s[3]])))
}

/// Whole seconds of audio, or `None` if the header does not say.
pub fn duration_secs(bytes: &[u8], file_len: u64) -> Option<u64> {
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        return wav(bytes, file_len);
    }
    if bytes.starts_with(b"fLaC") {
        return flac(bytes);
    }
    mp3(bytes, file_len)
}

/// `fmt ` gives the byte rate; `data` gives the payload size. A `data` chunk
/// past the prefix is estimated from the file size.
fn wav(b: &[u8], file_len: u64) -> Option<u64> {
    let mut pos = 12;
    let mut byte_rate: Option<u64> = None;
    while let (Some(id), Some(size)) = (b.get(pos..pos + 4), le32(b, pos + 4)) {
        match id {
            b"fmt " => byte_rate = Some(le32(b, pos + 8 + 8)?).filter(|r| *r > 0),
            b"data" => {
                let rate = byte_rate?;
                return Some(size / rate);
            }
            _ => {}
        }
        pos += 8 + usize::try_from(size).ok()? + (size % 2) as usize;
    }
    // `data` sits past what was read: the rest of the file is the payload.
    let rate = byte_rate?;
    Some(file_len.saturating_sub(pos as u64) / rate)
}

/// STREAMINFO is the first metadata block: sample rate (20 bits) and total
/// samples (36 bits) at fixed offsets.
fn flac(b: &[u8]) -> Option<u64> {
    if b.get(4)? & 0x7f != 0 {
        return None;
    }
    let s = b.get(18..26)?;
    let sample_rate = u64::from(s[0]) << 12 | u64::from(s[1]) << 4 | u64::from(s[2] >> 4);
    // Bits 35..0 of the eight: the low nibble of the fourth byte and the
    // four after it. Channels and bit depth sit between the rate and this.
    let total = u64::from(s[3] & 0x0f) << 32
        | u64::from(s[4]) << 24
        | u64::from(s[5]) << 16
        | u64::from(s[6]) << 8
        | u64::from(s[7]);
    (sample_rate > 0 && total > 0).then(|| total / sample_rate)
}

const BITRATES_V1_L3: [u64; 16] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
];
const BITRATES_V2_L3: [u64; 16] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
];

/// The first Layer III frame header, then a `Xing`/`Info` frame count if
/// there is one, else the constant-bitrate estimate.
fn mp3(b: &[u8], file_len: u64) -> Option<u64> {
    // An ID3v2 tag in front: 10-byte header, synchsafe size, optional footer.
    let mut pos = 0usize;
    if b.starts_with(b"ID3") {
        let s = b.get(6..10)?;
        let size = s
            .iter()
            .fold(0usize, |acc, x| (acc << 7) | usize::from(x & 0x7f));
        pos = 10 + size + if b[5] & 0x10 != 0 { 10 } else { 0 };
    }
    // Find the sync word.
    let start =
        (pos..b.len()).find(|&i| i + 3 < b.len() && b[i] == 0xff && b[i + 1] & 0xe0 == 0xe0)?;
    let (h1, h2, h3) = (b[start + 1], b[start + 2], b[start + 3]);
    let version = (h1 >> 3) & 3; // 3 = MPEG1, 2 = MPEG2, 0 = MPEG2.5
    let layer = (h1 >> 1) & 3; // 1 = Layer III
    if layer != 1 || version == 1 {
        return None;
    }
    let mpeg1 = version == 3;
    let bitrate_kbps = if mpeg1 {
        BITRATES_V1_L3[usize::from(h2 >> 4)]
    } else {
        BITRATES_V2_L3[usize::from(h2 >> 4)]
    };
    let sample_rate = match (version, (h2 >> 2) & 3) {
        (3, 0) => 44_100,
        (3, 1) => 48_000,
        (3, 2) => 32_000,
        (2, 0) => 22_050,
        (2, 1) => 24_000,
        (2, 2) => 16_000,
        (0, 0) => 11_025,
        (0, 1) => 12_000,
        (0, 2) => 8_000,
        _ => return None,
    };
    if bitrate_kbps == 0 {
        return None;
    }
    let samples_per_frame: u64 = if mpeg1 { 1152 } else { 576 };
    let mono = (h3 >> 6) & 3 == 3;
    let side_info = match (mpeg1, mono) {
        (true, true) => 17,
        (true, false) => 32,
        (false, true) => 9,
        (false, false) => 17,
    };
    let xing = start + 4 + side_info;
    if matches!(b.get(xing..xing + 4), Some(b"Xing" | b"Info")) {
        let flags = be32(b, xing + 4)?;
        if flags & 1 != 0 {
            let frames = be32(b, xing + 8)?;
            return Some(frames * samples_per_frame / sample_rate);
        }
    }
    let audio_bytes = file_len.saturating_sub(start as u64);
    Some(audio_bytes * 8 / (bitrate_kbps * 1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A canonical 44-byte WAV header for `secs` seconds at `byte_rate`.
    pub fn wav_header(byte_rate: u32, secs: u32) -> Vec<u8> {
        let data = byte_rate * secs;
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(36 + data).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&1u16.to_le_bytes()); // mono
        v.extend_from_slice(&8000u32.to_le_bytes()); // sample rate
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // block align
        v.extend_from_slice(&8u16.to_le_bytes()); // bits
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data.to_le_bytes());
        v
    }

    #[test]
    fn wav_divides_data_by_byte_rate() {
        let v = wav_header(8000, 2);
        assert_eq!(duration_secs(&v, 44 + 16_000), Some(2));
        // `data` cut off: estimated from the file size.
        assert_eq!(duration_secs(&v[..36], 36 + 24_000), Some(3));
        assert_eq!(duration_secs(&v[..20], 1000), None);
    }

    #[test]
    fn flac_reads_streaminfo() {
        let mut v = b"fLaC".to_vec();
        v.extend_from_slice(&[0x00, 0, 0, 34]); // STREAMINFO, 34 bytes
        v.extend_from_slice(&[0u8; 10]); // block sizes, frame sizes
                                         // 44100 Hz (20 bits), 2 channels-1 (3 bits), 16-1 bps (5 bits),
                                         // 441000 samples (36 bits).
        let sr: u64 = 44_100;
        let total: u64 = 441_000;
        let packed: u64 = (sr << 44) | (1 << 41) | (15 << 36) | total;
        v.extend_from_slice(&packed.to_be_bytes());
        assert_eq!(duration_secs(&v, 0), Some(10));
        assert_eq!(duration_secs(&v[..20], 0), None);
    }

    /// MPEG1 Layer III, 128 kbps, 44.1 kHz, stereo.
    fn mp3_frame_header() -> [u8; 4] {
        [0xff, 0xfb, 0x90, 0x00]
    }

    #[test]
    fn mp3_estimates_cbr_from_the_file_size() {
        let v = mp3_frame_header().to_vec();
        // 60 s at 128 kbps = 960 000 bytes.
        assert_eq!(duration_secs(&v, 960_000), Some(60));
        // Behind an ID3v2 tag of 100 bytes.
        let mut tagged = b"ID3\x04\x00\x00".to_vec();
        tagged.extend_from_slice(&[0, 0, 0, 100]);
        tagged.extend_from_slice(&[0u8; 100]);
        tagged.extend_from_slice(&mp3_frame_header());
        assert_eq!(duration_secs(&tagged, 110 + 960_000), Some(60));
    }

    #[test]
    fn mp3_prefers_the_xing_frame_count() {
        let mut v = mp3_frame_header().to_vec();
        v.extend_from_slice(&[0u8; 32]); // side info, stereo MPEG1
        v.extend_from_slice(b"Xing");
        v.extend_from_slice(&1u32.to_be_bytes()); // frames flag
                                                  // 30 s = 30 * 44100 / 1152 frames.
        let frames: u32 = 30 * 44_100 / 1152 + 1;
        v.extend_from_slice(&frames.to_be_bytes());
        assert_eq!(duration_secs(&v, 0), Some(30));
    }

    #[test]
    fn not_audio_is_none() {
        assert_eq!(duration_secs(b"hello", 100), None);
        assert_eq!(duration_secs(b"", 0), None);
    }
}
