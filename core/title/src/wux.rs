//! WUX: deduplicated, sector-indexed container for WUD disc images.
//!
//! ```text
//!  0x00 "WUX0"  0x04 0x1099D02E  0x08 sector size  0x10 uncompressed size (u64)
//!  0x20 index table: one u32 per WUD sector → stored sector number
//!  then stored sectors, aligned to the sector size
//! ```
//! All header fields are little-endian.
use std::io::{Read, Seek, SeekFrom};

use crate::TitleError;

const MAGIC0: u32 = 0x3058_5557; // "WUX0"
const MAGIC1: u32 = 0x1099_D02E;
const INDEX_OFFSET: u64 = 0x20;

/// Random-access reader over the decompressed WUD stream.
#[derive(Debug)]
pub struct WuxReader<R> {
    inner: R,
    sector_size: u64,
    uncompressed_size: u64,
    index: Vec<u32>,
    data_offset: u64,
}

impl<R: Read + Seek> WuxReader<R> {
    /// # Errors
    /// Fails if the header is not WUX or the index table is truncated.
    pub fn new(mut inner: R) -> Result<Self, TitleError> {
        let mut header = [0_u8; 0x20];
        inner.read_exact(&mut header).map_err(|_| TitleError::BadWuxMagic)?;

        let u32_at =
            |o: usize| u32::from_le_bytes([header[o], header[o + 1], header[o + 2], header[o + 3]]);

        if u32_at(0) != MAGIC0 || u32_at(4) != MAGIC1 {
            return Err(TitleError::BadWuxMagic);
        }

        let sector_size = u64::from(u32_at(8));
        let uncompressed_size = u64::from(u32_at(0x10)) | (u64::from(u32_at(0x14)) << 32);

        if sector_size == 0 {
            return Err(TitleError::CorruptWux);
        }

        let sector_count = usize::try_from(uncompressed_size.div_ceil(sector_size))
            .map_err(|_| TitleError::CorruptWux)?;
        let mut raw = vec![0_u8; sector_count * 4];

        inner.seek(SeekFrom::Start(INDEX_OFFSET)).map_err(|_| TitleError::CorruptWux)?;
        inner.read_exact(&mut raw).map_err(|_| TitleError::CorruptWux)?;

        let index = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let data_offset = (INDEX_OFFSET + raw.len() as u64).next_multiple_of(sector_size);

        Ok(Self { inner, sector_size, uncompressed_size, index, data_offset })
    }

    /// Size of the WUD image.
    #[inline]
    pub const fn len(&self) -> u64 {
        self.uncompressed_size
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.uncompressed_size == 0
    }

    /// Reads WUD bytes at `offset` into `buf`, returning the count read.
    ///
    /// # Errors
    /// Fails on I/O errors or an index entry pointing past the file.
    #[allow(clippy::cast_possible_truncation)]
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, TitleError> {
        let mut done = 0;

        while done < buf.len() {
            let position = offset + done as u64;
            if position >= self.uncompressed_size {
                break;
            }

            let sector = (position / self.sector_size) as usize;
            let within = position % self.sector_size;
            let stored = *self.index.get(sector).ok_or(TitleError::CorruptWux)?;
            let chunk = (self.sector_size - within)
                .min((buf.len() - done) as u64)
                .min(self.uncompressed_size - position);
            let file_offset = self.data_offset + u64::from(stored) * self.sector_size + within;

            self.inner.seek(SeekFrom::Start(file_offset)).map_err(|_| TitleError::CorruptWux)?;
            self.inner
                .read_exact(&mut buf[done..done + chunk as usize])
                .map_err(|_| TitleError::CorruptWux)?;
            done += chunk as usize;
        }
        Ok(done)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// Builds a WUX with sector size 0x40 and four WUD sectors, two of which are identical.
    fn sample() -> Vec<u8> {
        let sector = 0x40_usize;
        let mut file = vec![0_u8; 0x20];

        file[0..4].copy_from_slice(&MAGIC0.to_le_bytes());
        file[4..8].copy_from_slice(&MAGIC1.to_le_bytes());
        file[8..12].copy_from_slice(&(sector as u32).to_le_bytes());
        file[0x10..0x18].copy_from_slice(&(4 * sector as u64 - 8).to_le_bytes());

        for stored in [0_u32, 1, 0, 2] {
            file.extend_from_slice(&stored.to_le_bytes());
        }
        file.resize(file.len().next_multiple_of(sector), 0);

        for fill in [0xAA_u8, 0xBB, 0xCC] {
            file.extend(std::iter::repeat_n(fill, sector));
        }
        file
    }

    #[test]
    fn reads_across_deduplicated_sectors() {
        let mut wux = WuxReader::new(Cursor::new(sample())).unwrap();
        assert_eq!(wux.len(), 0xF8);

        {
            let mut buf = [0_u8; 4];
            wux.read_at(0x3E, &mut buf).unwrap();
            assert_eq!(buf, [0xAA, 0xAA, 0xBB, 0xBB]);

            wux.read_at(0x80, &mut buf).unwrap();
            assert_eq!(buf, [0xAA; 4]);
        }

        let mut tail = [0_u8; 16];
        assert_eq!(wux.read_at(0xF0, &mut tail).unwrap(), 8);
        assert_eq!(tail[..8], [0xCC; 8]);
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(matches!(
            WuxReader::new(Cursor::new(vec![0_u8; 64])),
            Err(TitleError::BadWuxMagic)
        ));
    }
}
