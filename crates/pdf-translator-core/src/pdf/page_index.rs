//! Page index newtype for safe conversion between usize and i32.
//!
//! This module provides a strongly-typed wrapper around page indices to ensure
//! safe conversion between Rust's usize (used for indexing) and mupdf's i32.

use std::fmt;

use crate::error::Error;

/// A page index that can be safely used with mupdf.
///
/// This newtype wraps an i32 and provides safe conversion from usize,
/// centralizing the conversion logic in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageIndex(i32);

impl PageIndex {
    /// Create a new PageIndex from an i32 value.
    ///
    /// This should only be used when you already have a valid i32 page index.
    #[must_use]
    pub const fn new(index: i32) -> Self {
        Self(index)
    }

    /// Get the underlying i32 value.
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self.0
    }

    /// Get the index as usize for Rust collections.
    ///
    /// Returns 0 if somehow the index is negative, though this should never happen
    /// if the PageIndex was created through `TryFrom<usize>` or `try_from_page_num`.
    #[must_use]
    #[allow(clippy::cast_sign_loss)] // Safe: we check for negative values
    pub const fn as_usize(self) -> usize {
        // PageIndex is always created from non-negative values, but we handle
        // the impossible case gracefully rather than panicking
        if self.0 < 0 {
            0
        } else {
            self.0 as usize
        }
    }

    /// Get the 1-indexed page number for lopdf (which uses 1-based indexing).
    ///
    /// Returns the page number as u32, suitable for use with lopdf's page APIs.
    #[must_use]
    pub const fn as_lopdf_page_number(self) -> u32 {
        // Safe because PageIndex is always non-negative and adding 1 won't overflow
        // for any realistic page count. Use cast_unsigned for explicitness.
        (self.0 + 1).cast_unsigned()
    }

    /// Try to create a PageIndex from a usize page number.
    ///
    /// Returns an error if the page number is too large to fit in an i32
    /// or exceeds the total page count.
    pub fn try_from_page_num(page_num: usize, total_pages: usize) -> Result<Self, Error> {
        if page_num >= total_pages {
            return Err(Error::PdfInvalidPage {
                page: page_num,
                total: total_pages,
            });
        }

        let index = i32::try_from(page_num).map_err(|_| Error::PdfInvalidPage {
            page: page_num,
            total: total_pages,
        })?;

        Ok(Self(index))
    }
}

impl TryFrom<usize> for PageIndex {
    type Error = Error;

    /// Convert a usize to a PageIndex.
    ///
    /// This conversion can fail if the value is too large to fit in an i32.
    /// For production use, prefer `try_from_page_num` which also validates
    /// against the document's page count.
    fn try_from(value: usize) -> Result<Self, Self::Error> {
        let index = i32::try_from(value).map_err(|_| Error::PdfInvalidPage {
            page: value,
            total: 0, // Unknown total when using raw conversion
        })?;
        Ok(Self(index))
    }
}

impl From<PageIndex> for i32 {
    fn from(index: PageIndex) -> Self {
        index.0
    }
}

impl fmt::Display for PageIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_page_index_creation() {
        let idx = PageIndex::new(5);
        assert_eq!(idx.as_i32(), 5);
        assert_eq!(idx.as_usize(), 5);
    }

    #[test]
    fn test_try_from_usize() {
        let idx = PageIndex::try_from(10_usize).unwrap();
        assert_eq!(idx.as_i32(), 10);
    }

    #[test]
    fn test_try_from_page_num_valid() {
        let idx = PageIndex::try_from_page_num(5, 10).unwrap();
        assert_eq!(idx.as_i32(), 5);
    }

    #[test]
    fn test_try_from_page_num_out_of_range() {
        let result = PageIndex::try_from_page_num(10, 5);
        assert!(result.is_err());
    }

    #[test]
    fn test_into_i32() {
        let idx = PageIndex::new(42);
        let value: i32 = idx.into();
        assert_eq!(value, 42);
    }

    #[test]
    fn test_display() {
        let idx = PageIndex::new(7);
        assert_eq!(format!("{idx}"), "7");
    }

    #[test]
    fn test_as_lopdf_page_number() {
        let idx = PageIndex::new(0);
        assert_eq!(idx.as_lopdf_page_number(), 1);

        let idx = PageIndex::new(5);
        assert_eq!(idx.as_lopdf_page_number(), 6);
    }
}
