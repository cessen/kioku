use std::alloc::Layout;

#[inline(always)]
pub(crate) fn alignment_offset(addr: usize, alignment: usize) -> usize {
    (alignment - (addr % alignment)) % alignment
}

/// Creates a new layout aligned to at least `align` bytes.
///
/// Panics if the resulting `Layout` would be invalid (e.g. non-power-of-two
/// alignment).
#[inline(always)]
pub(crate) fn min_alignment(layout: &Layout, align: usize) -> Layout {
    Layout::from_size_align(layout.size(), layout.align().max(align)).unwrap()
}

/// Currently `Layout::repeat()` is unstable in `std`, so we can't use it.
/// This is essentially a copy of that code, but it panics instead of returning
/// an error.  That difference aside, see the documentation of
/// `Layout::repeat()` for details.
///
/// The short version is: this is used for allocating arrays.
///
/// TODO: replace with a call to `Layout::repeat()` once that's stablized.
#[inline]
pub(crate) fn repeat_layout(layout: &Layout, n: usize) -> Layout {
    fn padding_needed_for(layout: &Layout, align: usize) -> usize {
        let len = layout.size();
        let len_rounded_up = len.wrapping_add(align).wrapping_sub(1) & !align.wrapping_sub(1);
        len_rounded_up.wrapping_sub(len)
    }

    let padded_size = layout
        .size()
        .checked_add(padding_needed_for(layout, layout.align()))
        .unwrap();
    let alloc_size = padded_size.checked_mul(n).unwrap();

    unsafe {
        // layout.align is already known to be valid and alloc_size has been
        // padded already.
        Layout::from_size_align_unchecked(alloc_size, layout.align())
    }
}
