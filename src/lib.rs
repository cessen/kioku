//! Kioku is a growable memory arena/pool

#![allow(clippy::redundant_field_names)]
#![allow(clippy::needless_return)]
#![allow(clippy::mut_from_ref)]
#![allow(clippy::transmute_ptr_to_ptr)]

use std::{
    alloc::Layout,
    cell::{Cell, RefCell},
    fmt,
    mem::{align_of, size_of, transmute, MaybeUninit},
    slice,
};

const GROWTH_FRACTION: usize = 8; // 1/N  (smaller number leads to bigger allocations)
const DEFAULT_INITIAL_BLOCK_SIZE: usize = 1 << 10; // 1 KiB
const DEFAULT_MAX_WASTE_PERCENTAGE: usize = 10;

#[inline(always)]
fn alignment_offset(addr: usize, alignment: usize) -> usize {
    (alignment - (addr % alignment)) % alignment
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
fn repeat_layout(layout: &Layout, n: usize) -> Layout {
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

/// Creates a new layout aligned to at least `align` bytes.
///
/// Panics if the resulting `Layout` would be invalid (e.g. non-power-of-two
/// alignment).
#[inline(always)]
fn min_alignment(layout: &Layout, align: usize) -> Layout {
    Layout::from_size_align(layout.size(), layout.align().max(align)).unwrap()
}

/// A growable memory arena for Copy types.
///
/// The arena works by allocating memory in blocks of slowly increasing size.
/// It doles out memory from the current block until an amount of memory is
/// requested that doesn't fit in the remainder of the current block, and then
/// allocates a new block.
///
/// Additionally, it attempts to minimize wasted space through some heuristics.
/// By default, it tries to keep memory waste within the arena below 10%.
#[derive(Default)]
pub struct Arena {
    blocks: RefCell<Vec<Vec<MaybeUninit<u8>>>>,
    min_block_size: usize,
    max_waste_percentage: usize,
    stat_space_occupied: Cell<usize>,
    stat_space_allocated: Cell<usize>,
}

impl fmt::Debug for Arena {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Arena")
            .field("blocks.len():", &self.blocks.borrow().len())
            .field("min_block_size", &self.min_block_size)
            .field("max_waste_percentage", &self.max_waste_percentage)
            .field("stat_space_occupied", &self.stat_space_occupied)
            .field("stat_space_allocated", &self.stat_space_allocated)
            .finish()
    }
}

impl Arena {
    /// Create a new arena, with default minimum block size.
    pub fn new() -> Arena {
        Arena {
            blocks: RefCell::new(vec![Vec::with_capacity(DEFAULT_INITIAL_BLOCK_SIZE)]),
            min_block_size: DEFAULT_INITIAL_BLOCK_SIZE,
            max_waste_percentage: DEFAULT_MAX_WASTE_PERCENTAGE,
            stat_space_occupied: Cell::new(DEFAULT_INITIAL_BLOCK_SIZE),
            stat_space_allocated: Cell::new(0),
        }
    }

    /// Create a new arena, with a specified initial block size.
    pub fn with_initial_block_size(initial_block_size: usize) -> Arena {
        assert!(initial_block_size > 0);

        Arena {
            blocks: RefCell::new(vec![Vec::with_capacity(initial_block_size)]),
            min_block_size: initial_block_size,
            max_waste_percentage: DEFAULT_MAX_WASTE_PERCENTAGE,
            stat_space_occupied: Cell::new(initial_block_size),
            stat_space_allocated: Cell::new(0),
        }
    }

    /// Create a new arena, with a specified initial block size and maximum
    /// waste percentage.
    pub fn with_settings(initial_block_size: usize, max_waste_percentage: usize) -> Arena {
        assert!(initial_block_size > 0);
        assert!(max_waste_percentage > 0 && max_waste_percentage <= 100);

        Arena {
            blocks: RefCell::new(vec![Vec::with_capacity(initial_block_size)]),
            min_block_size: initial_block_size,
            max_waste_percentage: max_waste_percentage,
            stat_space_occupied: Cell::new(initial_block_size),
            stat_space_allocated: Cell::new(0),
        }
    }

    /// Returns statistics about the current usage as a tuple:
    /// (space occupied, space allocated, block count, large block count)
    ///
    /// Space occupied is the amount of real memory that the Arena
    /// is taking up (not counting book keeping).
    ///
    /// Space allocated is the amount of the occupied space that is
    /// actually used.  In other words, it is the sum of the all the
    /// allocation requests made to the arena by client code.
    ///
    /// Block count is the number of blocks that have been allocated.
    pub fn stats(&self) -> (usize, usize, usize) {
        let occupied = self.stat_space_occupied.get();
        let allocated = self.stat_space_allocated.get();
        let blocks = self.blocks.borrow().len();

        (occupied, allocated, blocks)
    }

    /// Frees all memory currently allocated by the arena, resetting itself to
    /// start fresh.
    ///
    /// # Safety
    ///
    /// This is unsafe because it does NOT ensure that all references
    /// to the freed data are gone, so this can easily lead to dangling
    /// references to invalid memory.
    pub unsafe fn free_all_and_reset(&self) {
        let mut blocks = self.blocks.borrow_mut();

        blocks.clear();
        blocks.shrink_to_fit();
        blocks.push(Vec::with_capacity(self.min_block_size));

        self.stat_space_occupied.set(self.min_block_size);
        self.stat_space_allocated.set(0);
    }

    /// Allocates memory for and initializes a type T, returning a mutable
    /// reference to it.
    #[inline]
    pub fn alloc<T: Copy>(&self, value: T) -> &mut T {
        let memory = self.alloc_uninitialized();
        unsafe {
            *memory.as_mut_ptr() = value;
        }
        unsafe { transmute(memory) }
    }

    /// Allocates memory for and initializes a type T, returning a mutable
    /// reference to it.
    ///
    /// Additionally, the allocation will be made with the given byte alignment
    /// or the type's inherent alignment, whichever is greater.
    #[inline]
    pub fn alloc_with_alignment<T: Copy>(&self, value: T, align: usize) -> &mut T {
        let memory = self.alloc_uninitialized_with_alignment(align);
        unsafe {
            *memory.as_mut_ptr() = value;
        }
        unsafe { transmute(memory) }
    }

    /// Allocates memory for a type `T`, returning a mutable reference to it.
    ///
    /// CAUTION: the memory returned is uninitialized.  Make sure to initalize
    /// before using!
    #[inline]
    pub fn alloc_uninitialized<T: Copy>(&self) -> &mut MaybeUninit<T> {
        assert!(
            size_of::<T>() > 0,
            "`Arena` does not support zero-sized types."
        );

        let memory = self.alloc_raw(&Layout::new::<T>()) as *mut MaybeUninit<T>;

        unsafe { memory.as_mut().unwrap() }
    }

    /// Allocates memory for a type `T`, returning a mutable reference to it.
    ///
    /// Additionally, the allocation will be made with the given byte alignment
    /// or the type's inherent alignment, whichever is greater.
    ///
    /// CAUTION: the memory returned is uninitialized.  Make sure to initalize
    /// before using!
    #[inline]
    pub fn alloc_uninitialized_with_alignment<T: Copy>(&self, align: usize) -> &mut MaybeUninit<T> {
        assert!(
            size_of::<T>() > 0,
            "`Arena` does not support zero-sized types."
        );

        let layout = min_alignment(&Layout::new::<T>(), align);
        let memory = self.alloc_raw(&layout) as *mut MaybeUninit<T>;
        unsafe { memory.as_mut().unwrap() }
    }

    /// Allocates memory for `len` values of type `T`, returning a mutable
    /// slice to it.  All elements are initialized to the given `value`.
    #[inline]
    pub fn alloc_array<T: Copy>(&self, len: usize, value: T) -> &mut [T] {
        let memory = self.alloc_array_uninitialized(len);

        for v in memory.iter_mut() {
            unsafe {
                *v.as_mut_ptr() = value;
            }
        }

        unsafe { transmute(memory) }
    }

    /// Allocates memory for `len` values of type `T`, returning a mutable
    /// slice to it. All elements are initialized to the given `value`.
    ///
    /// Additionally, the allocation will be made with the given byte alignment
    /// or the type's inherent alignment, whichever is greater.
    #[inline]
    pub fn alloc_array_with_alignment<T: Copy>(
        &self,
        len: usize,
        value: T,
        align: usize,
    ) -> &mut [T] {
        let memory = self.alloc_array_uninitialized_with_alignment(len, align);

        for v in memory.iter_mut() {
            unsafe {
                *v.as_mut_ptr() = value;
            }
        }

        unsafe { transmute(memory) }
    }

    /// Allocates and initializes memory to duplicate the given slice,
    /// returning a mutable slice to the new copy.
    ///
    /// # Panics
    ///
    /// Panics if `T` is zero-sized (unsupported).
    #[inline]
    pub fn copy_slice<T: Copy>(&self, other: &[T]) -> &mut [T] {
        let memory = self.alloc_array_uninitialized(other.len());

        for (v, other) in memory.iter_mut().zip(other.iter()) {
            unsafe {
                *v.as_mut_ptr() = *other;
            }
        }

        unsafe { transmute(memory) }
    }

    /// Allocates and initializes memory to duplicate the given slice,
    /// returning a mutable slice to the new copy.
    ///
    /// Additionally, the start of array itself will be aligned to at least the
    /// given byte alignment.
    ///
    /// # Panics
    ///
    /// Panics if `T` is zero-sized (unsupported).
    #[inline]
    pub fn copy_slice_with_alignment<T: Copy>(&self, other: &[T], align: usize) -> &mut [T] {
        let memory = self.alloc_array_uninitialized_with_alignment(other.len(), align);

        for (v, other) in memory.iter_mut().zip(other.iter()) {
            unsafe {
                *v.as_mut_ptr() = *other;
            }
        }

        unsafe { transmute(memory) }
    }

    /// Allocates uninitialized memory for `len` values of type `T`, returning
    /// a mutable slice to it.
    ///
    /// # Panics
    ///
    /// Panics if `T` is zero-sized (unsupported).
    #[inline]
    pub fn alloc_array_uninitialized<T: Copy>(&self, len: usize) -> &mut [MaybeUninit<T>] {
        self.alloc_array_uninitialized_with_alignment(len, align_of::<T>())
    }

    /// Allocates uninitialized memory for `len` values of type `T`, returning
    /// a mutable uninitialized slice to it.
    ///
    /// Additionally, the start of array itself will be aligned to at least the
    /// given byte alignment.
    ///
    /// # Panics
    ///
    /// Panics if `T` is zero-sized (unsupported).
    #[inline]
    pub fn alloc_array_uninitialized_with_alignment<T: Copy>(
        &self,
        len: usize,
        array_start_alignment: usize,
    ) -> &mut [MaybeUninit<T>] {
        assert!(
            size_of::<T>() > 0,
            "`Arena` does not support zero-sized types."
        );

        let layout = min_alignment(
            &repeat_layout(&Layout::new::<T>(), len),
            array_start_alignment,
        );
        let memory = self.alloc_raw(&layout) as *mut MaybeUninit<T>;
        unsafe { slice::from_raw_parts_mut(memory, len) }
    }

    /// Allocates space with the given memory layout.
    ///
    /// Returns a mutable pointer to the start of the uninitialized bytes.
    ///
    /// # Safety
    ///
    /// Although this function is not itself unsafe, it is very easy to
    /// accidentally do unsafe things with the returned pointer.  In
    /// particular, only memory within the size of the requested layout is
    /// valid, and the returned allocation is only valid for as long as the
    /// `Arena` itself is.  The other methods on `Arena` protect against this
    /// by using references or slices with appropriate lifetimes.
    fn alloc_raw(&self, layout: &Layout) -> *mut MaybeUninit<u8> {
        let alignment = layout.align();
        let size = layout.size();

        self.stat_space_allocated
            .set(self.stat_space_allocated.get() + size); // Update stats

        let mut blocks = self.blocks.borrow_mut();

        // If it's a zero-size allocation, just point to the beginning of the current block.
        if size == 0 {
            return blocks.first_mut().unwrap().as_mut_ptr();
        }
        // If it's non-zero-size.
        else {
            let start_index = {
                let block_addr = blocks.first().unwrap().as_ptr() as usize;
                let block_filled = blocks.first().unwrap().len();
                block_filled + alignment_offset(block_addr + block_filled, alignment)
            };

            // If it will fit in the current block, use the current block.
            if (start_index + size) <= blocks.first().unwrap().capacity() {
                unsafe {
                    blocks.first_mut().unwrap().set_len(start_index + size);
                }

                let block_ptr = blocks.first_mut().unwrap().as_mut_ptr();
                return unsafe { block_ptr.add(start_index) };
            }
            // If it won't fit in the current block, create a new block and use that.
            else {
                let next_size = if blocks.len() >= GROWTH_FRACTION {
                    let a = self.stat_space_occupied.get() / GROWTH_FRACTION;
                    let b = a % self.min_block_size;
                    if b > 0 {
                        a - b + self.min_block_size
                    } else {
                        a
                    }
                } else {
                    self.min_block_size
                };

                let waste_percentage = {
                    let w1 =
                        ((blocks[0].capacity() - blocks[0].len()) * 100) / blocks[0].capacity();
                    let w2 = ((self.stat_space_occupied.get() - self.stat_space_allocated.get())
                        * 100)
                        / self.stat_space_occupied.get();
                    if w1 < w2 {
                        w1
                    } else {
                        w2
                    }
                };

                // If it's a "large allocation", give it its own memory block.
                if (size + alignment) > next_size || waste_percentage > self.max_waste_percentage {
                    // Update stats
                    self.stat_space_occupied
                        .set(self.stat_space_occupied.get() + size + alignment - 1);

                    blocks.push(Vec::with_capacity(size + alignment - 1));
                    unsafe {
                        blocks.last_mut().unwrap().set_len(size + alignment - 1);
                    }

                    let start_index =
                        alignment_offset(blocks.last().unwrap().as_ptr() as usize, alignment);

                    let block_ptr = blocks.last_mut().unwrap().as_mut_ptr();
                    return unsafe { block_ptr.add(start_index) };
                }
                // Otherwise create a new shared block.
                else {
                    // Update stats
                    self.stat_space_occupied
                        .set(self.stat_space_occupied.get() + next_size);

                    blocks.push(Vec::with_capacity(next_size));
                    let block_count = blocks.len();
                    blocks.swap(0, block_count - 1);

                    let start_index =
                        alignment_offset(blocks.first().unwrap().as_ptr() as usize, alignment);

                    unsafe {
                        blocks.first_mut().unwrap().set_len(start_index + size);
                    }

                    let block_ptr = blocks.first_mut().unwrap().as_mut_ptr();
                    return unsafe { block_ptr.add(start_index) };
                }
            }
        }
    }
}
