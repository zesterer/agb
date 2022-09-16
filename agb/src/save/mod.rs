//! Module for reading and writing to save media.
//!
//! ## Save media types
//!
//! There are, broadly speaking, three different kinds of save media that can be
//! found in official Game Carts:
//!
//! * Battery-Backed SRAM: The simplest kind of save media, which can be accessed
//!   like normal memory. You can have SRAM up to 32KiB, and while there exist a
//!   few variants this does not matter much for a game developer.
//! * EEPROM: A kind of save media based on very cheap chips and slow chips.
//!   These are accessed using a serial interface based on reading/writing bit
//!   streams into IO registers. This memory comes in 8KiB and 512 byte versions,
//!   which unfortunately cannot be distinguished at runtime.
//! * Flash: A kind of save media based on flash memory. Flash memory can be read
//!   like ordinary memory, but writing requires sending commands using multiple
//!   IO register spread across the address space. This memory comes in 64KiB
//!   and 128KiB variants, which can thankfully be distinguished using a chip ID.
//!
//! As these various types of save media cannot be easily distinguished at
//! runtime, the kind of media in use should be set manually.
//!
//! ## Setting save media type
//!
//! To use save media in your game, you must set which type to use. This is done
//! by calling one of the following functions at startup:
//!
//! * For 32 KiB battery-backed SRAM, call [`use_sram`].
//! * For 64 KiB flash memory, call [`use_flash_64k`].
//! * For 128 KiB flash memory, call [`use_flash_128k`].
//! * For 512 byte EEPROM, call [`use_eeprom_512b`].
//! * For 8 KiB EEPROM, call [`use_eeprom_8k`].
//!
//! TODO Update example
//! ```rust,norun
//! # use gba::save;
//! save::use_flash_128k();
//! save::set_timer_for_timeout(3); // Uses timer 3 for save media timeouts.
//! ```
//!
//! ## Using save media
//!
//! To access save media, use the [`SaveData::new`] method to create a new
//! [`SaveData`] object. Its methods are used to read or write save media.
//!
//! Reading data from the savegame is simple. Use [`read`] to copy data from an
//! offset in the savegame into a buffer in memory.
//!
//! TODO Update example
//! ```rust,norun
//! # use gba::{info, save::SaveAccess};
//! let mut buf = [0; 1000];
//! SaveAccess::new()?.read(1000, &mut buf)?;
//! info!("Memory result: {:?}", buf);
//! ```
//!
//! Writing to save media requires you to prepare the area for writing by calling
//! the [`prepare_write`] method to return a [`SavePreparedBlock`], which contains
//! the actual [`write`] method.
//!
//! TODO Update example
//! ```rust,norun
//! # use gba::{info, save::SaveAccess};
//! let access = SaveAccess::new()?;
//! access.prepare_write(500..600)?;
//! access.write(500, &[10; 25])?;
//! access.write(525, &[20; 25])?;
//! access.write(550, &[30; 25])?;
//! access.write(575, &[40; 25])?;
//! ```
//!
//! The `prepare_write` method leaves everything in a sector that overlaps the
//! range passed to it in an implementation defined state. On some devices it may
//! do nothing, and on others, it may clear the entire range to `0xFF`.
//!
//! Because writes can only be prepared on a per-sector basis, a clear on a range
//! of `4000..5000` on a device with 4096 byte sectors will actually clear a range
//! of `0..8192`. Use [`sector_size`] to find the sector size, or [`align_range`]
//! to directly calculate the range of memory that will be affected by the clear.
//!
//! [`read`]: SaveData::read
//! [`prepare_write`]: SaveData::prepare_write
//! [`write`]: SavePreparedBlock::write
//! [`sector_size`]: SaveAccess::sector_size
//! [`align_range`]: SaveAccess::align_range
//!
//! ## Performance and Other Details
//!
//! The performance characteristics of the media types are as follows:
//!
//! * SRAM is simply a form of battery backed memory, and has no particular
//!   performance characteristics.  Reads and writes at any alignment are
//!   efficient. Furthermore, no timer is needed for accesses to this type of
//!   media. `prepare_write` does not immediately erase any data.
//! * Non-Atmel flash chips have a sector size of 4096 bytes. Reads and writes
//!   to any alignment are efficient, however, `prepare_write` will erase all
//!   data in an entire sector before writing.
//! * Atmel flash chips have a sector size of 128 bytes. Reads to any alignment
//!   are efficient, however, unaligned writes are extremely slow.
//!   `prepare_write` does not immediately erase any data.
//! * EEPROM has a sector size of 8 bytes. Unaligned reads and writes are
//!   slower than aligned writes, however, this is easily mitigated by the
//!   small sector size.

use core::ops::Range;
use crate::sync::{Mutex, RawMutexGuard};
use crate::timer::Timer;

mod asm_utils;
//mod setup;
mod utils;

//pub use asm_utils::*;
//pub use setup::*;

//pub mod eeprom;
//pub mod flash;
//pub mod sram;

/// A list of save media types.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub enum MediaType {
  /// 32KiB Battery-Backed SRAM or FRAM
  Sram32K,
  /// 8KiB EEPROM
  Eeprom8K,
  /// 512B EEPROM
  Eeprom512B,
  /// 64KiB flash chip
  Flash64K,
  /// 128KiB flash chip
  Flash128K,
  /// A user-defined save media type
  Custom,
}

/// The type used for errors encountered while reading or writing save media.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Error {
  /// There is no save media attached to this game cart.
  NoMedia,
  /// Failed to write the data to save media.
  WriteError,
  /// An operation on save media timed out.
  OperationTimedOut,
  /// An attempt was made to access save media at an invalid offset.
  OutOfBounds,
  /// The media is already in use.
  ///
  /// This can generally only happen in an IRQ that happens during an ongoing
  /// save media operation.
  MediaInUse,
  /// This command cannot be used with the save media in use.
  IncompatibleCommand,
}

/// Information about the save media used.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct MediaInfo {
  /// The type of save media installed.
  pub media_type: MediaType,
  /// The power-of-two size of each sector. Zero represents a sector size of
  /// 0, implying sectors are not in use.
  ///
  /// (For example, 512 byte sectors would return 9 here.)
  pub sector_shift: usize,
  /// The size of the save media, in sectors.
  pub sector_count: usize,
  /// Whether the save media type requires media be prepared before writing.
  pub uses_prepare_write: bool,
}

/// A trait allowing low-level saving and writing to save media.
trait RawSaveAccess: Sync {
  fn info(&self) -> Result<&'static MediaInfo, Error>;
  fn read(&self, offset: usize, buffer: &mut [u8]) -> Result<(), Error>;
  fn verify(&self, offset: usize, buffer: &[u8]) -> Result<bool, Error>;
  fn prepare_write(&self, sector: usize, count: usize) -> Result<(), Error>;
  fn write(&self, offset: usize, buffer: &[u8]) -> Result<(), Error>;
}

static CURRENT_SAVE_ACCESS: Mutex<Option<&'static dyn RawSaveAccess>> = Mutex::new(None);

fn set_save_implementation(access_impl: &'static dyn RawSaveAccess) {
  let mut access = CURRENT_SAVE_ACCESS.lock();
  assert!(access.is_none(), "Cannot initialize the savegame engine more than once.");
  *access = Some(access_impl);
}

fn get_save_implementation() -> Option<&'static dyn RawSaveAccess> {
  *CURRENT_SAVE_ACCESS.lock()
}

/// Allows reading and writing of save media.
#[derive(Copy, Clone)]
pub struct SaveData {
  lock: RawMutexGuard<'static>,
  access: &'static dyn RawSaveAccess,
  info: &'static MediaInfo,
  timeout: utils::Timeout,
}
impl SaveData {
  /// Creates a new save accessor around the current save implementaiton.
  fn new(timer: Option<Timer>) -> Result<SaveData, Error> {
    match get_save_implementation() {
      Some(access) => Ok(SaveData {
        lock: utils::lock_media()?,
        access,
        info: access.info()?,
        timeout: utils::Timeout::new(timer),
      }),
      None => Err(Error::NoMedia),
    }
  }

  /// Returns the media info underlying this accessor.
  pub fn media_info(&self) -> &'static MediaInfo {
    self.info
  }

  /// Returns the save media type being used.
  pub fn media_type(&self) -> MediaType {
    self.info.media_type
  }

  /// Returns the sector size of the save media. It is generally optimal to
  /// write data in blocks that are aligned to the sector size.
  pub fn sector_size(&self) -> usize {
    1 << self.info.sector_shift
  }

  /// Returns the total length of this save media.
  pub fn len(&self) -> usize {
    self.info.sector_count << self.info.sector_shift
  }

  fn check_bounds(&self, range: Range<usize>) -> Result<(), Error> {
    if range.start >= self.len() || range.end >= self.len() {
      Err(Error::OutOfBounds)
    } else {
      Ok(())
    }
  }
  fn check_bounds_len(&self, offset: usize, len: usize) -> Result<(), Error> {
    self.check_bounds(offset..(offset + len))
  }

  /// Copies data from the save media to a buffer.
  ///
  /// If an error is returned, the contents of the buffer are unpredictable.
  pub fn read(&self, offset: usize, buffer: &mut [u8]) -> Result<(), Error> {
    self.check_bounds_len(offset, buffer.len())?;
    self.access.read(offset, buffer)
  }

  /// Verifies that a given block of memory matches the save media.
  pub fn verify(&self, offset: usize, buffer: &[u8]) -> Result<bool, Error> {
    self.check_bounds_len(offset, buffer.len())?;
    self.access.verify(offset, buffer)
  }

  /// Returns a range that contains all sectors the input range overlaps.
  ///
  /// This can be used to calculate which blocks would be erased by a call
  /// to [`prepare_write`](`SaveAccess::prepare_write`)
  pub fn align_range(&self, range: Range<usize>) -> Range<usize> {
    let shift = self.info.sector_shift;
    let mask = (1 << shift) - 1;
    (range.start & !mask)..((range.end + mask) & !mask)
  }

  /// Prepares a given span of offsets for writing.
  ///
  /// This will erase any data in any sector overlapping the input range. To
  /// calculate which offset ranges would be affected, use the
  /// [`align_range`](`SaveAccess::align_range`) function.
  pub fn prepare_write(&mut self, range: Range<usize>) -> Result<SavePreparedBlock, Error> {
    self.check_bounds(range.clone())?;
    if self.info.uses_prepare_write {
      let range = self.align_range(range.clone());
      let shift = self.info.sector_shift;
      self.access.prepare_write(range.start >> shift, range.len() >> shift)?;
    }
    Ok(SavePreparedBlock {
      parent: self,
      range
    })
  }
}

/// A block of save memory that has been prepared for writing.
pub struct SavePreparedBlock<'a> {
  parent: &'a mut SaveData,
  range: Range<usize>,
}
impl<'a> SavePreparedBlock<'a> {
  /// Writes a given buffer into the save media.
  ///
  /// Multiple overlapping writes to the same memory range without a separate
  /// call to `prepare_write` will leave the save data in an unpredictable
  /// state. If an error is returned, the contents of the save media is
  /// unpredictable.
  pub fn write(&self, offset: usize, buffer: &[u8]) -> Result<(), Error> {
    if buffer.len() == 0 {
      Ok(())
    } else if !self.range.contains(&offset) ||
        !self.range.contains(&(offset + buffer.len() - 1)) {
      Err(Error::OutOfBounds)
    } else {
      self.parent.access.write(offset, buffer)
    }
  }

  /// Writes and validates a given buffer into the save media.
  ///
  /// This function will verify that the write has completed successfully, and
  /// return an error if it has not done so.
  ///
  /// Multiple overlapping writes to the same memory range without a separate
  /// call to `prepare_write` will leave the save data in an unpredictable
  /// state. If an error is returned, the contents of the save media is
  /// unpredictable.
  pub fn write_and_verify(&self, offset: usize, buffer: &[u8]) -> Result<(), Error> {
    self.write(offset, buffer)?;
    if !self.parent.verify(offset, buffer)? {
      Err(Error::WriteError)
    } else {
      Ok(())
    }
  }
}

/// Allows access to the cartridge's save data.
pub struct SaveManager;
impl SaveManager {
  pub fn access() -> Result<SaveData, Error> {
    SaveData::new(None)
  }
  pub fn access_with_timer(timer: Timer) -> Result<SaveData, Error> {
    SaveData::new(Some(timer))
  }
}