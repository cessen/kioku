//! Kioku is a memory arena allocator for Rust.
//!
//! The arena works by internally allocating memory in large-ish blocks of
//! memory one-at-a-time, and doling out memory from the current block in
//! linear order until its space runs out.
//!
//! Additionally, it attempts to minimize wasted space through some heuristics
//! based on a configurable maximum waste percentage.
//!
//! Some contrived example usage:
//!
//! ```rust
//! # use kioku::Arena;
//! let arena = Arena::new().with_block_size(1024);
//!
//! let integer = arena.alloc(42);
//! let array1 = arena.copy_slice(&[1, 2, 3, 4, 5, 42]);
//! assert_eq!(*integer, array1[5]);
//!
//! *integer = 16;
//! array1[1] = 16;
//! assert_eq!(*integer, array1[1]);
//!
//! let character = arena.alloc('A');
//! let array2 = arena.alloc_array('A', 42);
//! assert_eq!(array2.len(), 42);
//! assert_eq!(*character, array2[20]);
//!
//! *character = '学';
//! array2[30] = '学';
//! assert_eq!(*character, array2[30]);
//! ```
//!
//! # Large Allocations
//!
//! Allocations larger than the block size are handled by just allocating them
//! separately.  Those large allocations are also owned by the arena, just like
//! all other arena allocations, and will be freed when it gets dropped.
//!
//! # Custom Alignment
//!
//! All methods with a custom alignment parameter require the alignment to be
//! greater than zero and a power of two.  Moreover, the alignment parameter
//! can only increase the strictness of the alignment, and will be ignored if
//! less strict than the natural alignment of the type being allocated.
//!
//! Array allocation methods with alignment parameters only align the head of
//! the array to that alignment, and otherwise follow standard array memory
//! layout.
//!
//! # Zero Sized Types
//!
//! Zero-sized types such as `()` are unsupported.  All allocations will panic
//! if `T` is zero-sized.
//!
//! However, you *can* allocate zero length arrays using the array allocation
//! methods.  Only `T` itself must be non-zero-sized.

// Normally I agree with this lint, but in this particular library's case it
// just gets too noisy not using transmute.  It actually obscures intent when
// reading the code.
#![allow(clippy::transmute_ptr_to_ptr)]
// Disabling this particular clippy warning requires more significant
// explaination.
//
// If you look at the lint's docs, it says that this is "trivially unsound".
// And yet we're doing it _all over the place_ in this library.  In public
// APIs, no less.  So what's up?
//
// The reason violating this lint is _usually_ trivially unsound is that it
// allows returning multiple mutable references to _the same memory_.  However,
// in the case of this library, every call to these methods returns a mutable
// reference to a _new and different_ piece of memory.  Every time.  In fact,
// that's the whole point: it's an allocator.  So in our case, this actually is
// sound.  Thus, disabling the lint.
#![allow(clippy::mut_from_ref)]

use std::{
    alloc::Layout,
    cell::{Cell, RefCell},
    collections::LinkedList,
    fmt,
    mem::{size_of, transmute, MaybeUninit},
    slice,
};

/// A memory arena allocator.
#[derive(Default)]
pub struct Arena {
    blocks: RefCell<LinkedList<Vec<MaybeUninit<u8>>>>,
    min_block_size: usize,
    growth_strategy: GrowthStrategy,
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
    /// Create a new arena with default settings.
    ///
    /// - Initial block size: 1 KiB
    /// - Growth strategy: constant
    /// - Maximum waste percentage: 20 percent
    pub fn new() -> Arena {
        Arena {
            blocks: RefCell::new(LinkedList::new()),
            min_block_size: 1 << 10, // 1 KiB,
            growth_strategy: GrowthStrategy::Constant,
            max_waste_percentage: 20,
            stat_space_occupied: Cell::new(0),
            stat_space_allocated: Cell::new(0),
        }
    }

    /// Build an arena with a specified block size in bytes.
    pub fn with_block_size(self, block_size: usize) -> Arena {
        assert!(
            block_size > 0,
            "Initial block size must be greater \
             than zero"
        );
        assert!(
            self.blocks.borrow().is_empty(),
            "Cannot change initial block size after \
             blocks have already been allocated"
        );

        Arena {
            min_block_size: block_size,
            ..self
        }
    }

    /// Build an arena with a specified maximum waste percentage.
    ///
    /// - Recommended values are between 10 and 30.
    /// - 100 disables waste minimization entirely, which may be appropriate for
    ///   some use-cases.
    /// - Values close to 0 are absolutely _not_ recommended, as that will
    ///   likely trigger a lot of one-off non-arena allocations even for small
    ///   allocation requests, which defeats the whole purpose of using a memory
    ///   arena.
    pub fn with_max_waste_percentage(self, max_waste_percentage: usize) -> Arena {
        assert!(
            max_waste_percentage > 0 && max_waste_percentage <= 100,
            "The max waste percentage must be between 1 and 100"
        );

        Arena {
            max_waste_percentage,
            ..self
        }
    }

    /// Build an arena with a specified memory block growth strategy.
    pub fn with_growth_strategy(self, growth_strategy: GrowthStrategy) -> Arena {
        Arena {
            growth_strategy,
            ..self
        }
    }

    //------------------------------------------------------------------------
    // Basic methods

    /// Allocates a `T` initialized to `value`
    #[inline]
    pub fn alloc<T: Copy>(&self, value: T) -> &mut T {
        let memory = self.alloc_uninit();
        unsafe {
            *memory.as_mut_ptr() = value;
        }
        unsafe { transmute(memory) }
    }

    /// Allocates a `[T]` with all elements initialized to `value`.
    #[inline]
    pub fn alloc_array<T: Copy>(&self, value: T, len: usize) -> &mut [T] {
        let memory = self.alloc_array_uninit(len);

        for v in memory.iter_mut() {
            unsafe {
                *v.as_mut_ptr() = value;
            }
        }

        unsafe { transmute(memory) }
    }

    /// Allocates a `[T]` initialized to the contents of `slice`.
    #[inline]
    pub fn copy_slice<T: Copy>(&self, slice: &[T]) -> &mut [T] {
        let memory = self.alloc_array_uninit(slice.len());

        for (v, slice_item) in memory.iter_mut().zip(slice.iter()) {
            unsafe {
                *v.as_mut_ptr() = *slice_item;
            }
        }

        unsafe { transmute(memory) }
    }

    /// Allocates a `str` initialized to the contents of `text`.
    #[inline]
    pub fn copy_str(&self, text: &str) -> &mut str {
        let memory = self.alloc_array_uninit::<u8>(text.len());

        for (byte, text_byte) in memory.iter_mut().zip(text.as_bytes().iter()) {
            unsafe {
                *byte.as_mut_ptr() = *text_byte;
            }
        }

        unsafe { std::str::from_utf8_unchecked_mut(transmute(memory)) }
    }

    //------------------------------------------------------------------------
    // Initialized allocation methods with alignment.

    /// Allocates a `T` initialized to `value`, aligned to at least `align`
    /// bytes.
    #[inline]
    pub fn alloc_align<T: Copy>(&self, value: T, align: usize) -> &mut T {
        let memory = self.alloc_align_uninit(align);
        unsafe {
            *memory.as_mut_ptr() = value;
        }
        unsafe { transmute(memory) }
    }

    /// Allocates a `[T]` with all elements initialized to `value`, aligned to
    /// at least `align` bytes.
    #[inline]
    pub fn alloc_array_align<T: Copy>(&self, value: T, len: usize, align: usize) -> &mut [T] {
        let memory = self.alloc_array_align_uninit(len, align);

        for v in memory.iter_mut() {
            unsafe {
                *v.as_mut_ptr() = value;
            }
        }

        unsafe { transmute(memory) }
    }

    /// Allocates a `[T]` initialized to the contents of `slice`, aligned to at
    /// least `align` bytes.
    #[inline]
    pub fn copy_slice_align<T: Copy>(&self, slice: &[T], align: usize) -> &mut [T] {
        let memory = self.alloc_array_align_uninit(slice.len(), align);

        for (v, slice_item) in memory.iter_mut().zip(slice.iter()) {
            unsafe {
                *v.as_mut_ptr() = *slice_item;
            }
        }

        unsafe { transmute(memory) }
    }

    //------------------------------------------------------------------------
    // Uninitialized allocation methods.

    /// Allocates an uninitialized `T`.
    #[inline]
    pub fn alloc_uninit<T: Copy>(&self) -> &mut MaybeUninit<T> {
        assert!(
            size_of::<T>() > 0,
            "`Arena` does not support zero-sized types."
        );

        let memory = self.alloc_raw(&Layout::new::<T>()) as *mut MaybeUninit<T>;

        unsafe { memory.as_mut().unwrap() }
    }

    /// Allocates a uninitialized `[T]`.
    #[inline]
    pub fn alloc_array_uninit<T: Copy>(&self, len: usize) -> &mut [MaybeUninit<T>] {
        assert!(
            size_of::<T>() > 0,
            "`Arena` does not support zero-sized types."
        );

        let layout = Layout::array::<T>(len).unwrap();
        let memory = self.alloc_raw(&layout) as *mut MaybeUninit<T>;
        unsafe { slice::from_raw_parts_mut(memory, len) }
    }

    /// Allocates an uninitialized `T`, aligned to at least `align` bytes.
    #[inline]
    pub fn alloc_align_uninit<T: Copy>(&self, align: usize) -> &mut MaybeUninit<T> {
        assert!(
            size_of::<T>() > 0,
            "`Arena` does not support zero-sized types."
        );
        assert!(
            align.is_power_of_two(),
            "Invalid alignment: not a power of two."
        );

        let layout = Layout::new::<T>().align_to(align).unwrap();
        let memory = self.alloc_raw(&layout) as *mut MaybeUninit<T>;
        unsafe { memory.as_mut().unwrap() }
    }

    /// Allocates a uninitialized `[T]`, aligned to at least `align` bytes.
    #[inline]
    pub fn alloc_array_align_uninit<T: Copy>(
        &self,
        len: usize,
        align: usize,
    ) -> &mut [MaybeUninit<T>] {
        assert!(
            size_of::<T>() > 0,
            "`Arena` does not support zero-sized types."
        );
        assert!(
            align.is_power_of_two(),
            "Invalid alignment: not a power of two."
        );

        let layout = Layout::array::<T>(len).unwrap().align_to(align).unwrap();
        let memory = self.alloc_raw(&layout) as *mut MaybeUninit<T>;
        unsafe { slice::from_raw_parts_mut(memory, len) }
    }

    //------------------------------------------------------------------------
    // Raw work-horse allocation method.

    /// Allocates uninitialized memory with the given layout.
    ///
    /// # Safety
    ///
    /// Although this function is not itself unsafe, it is very easy to
    /// accidentally do unsafe things with the returned pointer.
    ///
    /// In particular, only memory within the size of the requested layout is
    /// valid, and the returned allocation is only valid for as long as the
    /// `Arena` itself is.  The other allocation methods all protect against
    /// those issues by returning references or slices with appropriate
    /// lifetimes.
    pub fn alloc_raw(&self, layout: &Layout) -> *mut MaybeUninit<u8> {
        #[inline(always)]
        fn alignment_offset(addr: usize, alignment: usize) -> usize {
            (alignment - (addr % alignment)) % alignment
        }

        let alignment = layout.align();
        let size = layout.size();

        let mut blocks = self.blocks.borrow_mut();

        // Add the first block if we're empty.
        if blocks.is_empty() {
            blocks.push_front(Vec::with_capacity(self.min_block_size));

            // Update stats
            self.stat_space_occupied
                .set(self.stat_space_occupied.get() + self.min_block_size);
        }

        // If we're zero-sized, just put us at the start of the current block.
        if size == 0 {
            return blocks.front_mut().unwrap().as_mut_ptr();
        }

        // Find our starting index for if we're allocating in the current block.
        let start_index_proposal = {
            let cur_block = blocks.front().unwrap();
            let block_addr = cur_block.as_ptr() as usize;
            let block_filled = cur_block.len();
            block_filled + alignment_offset(block_addr + block_filled, alignment)
        };

        // If it will fit in the current block, use the current block.
        if (start_index_proposal + size) <= blocks.front().unwrap().capacity() {
            let cur_block = blocks.front_mut().unwrap();

            // Do the bump allocation.
            let new_len = (start_index_proposal + size).max(cur_block.len());
            unsafe { cur_block.set_len(new_len) };

            // Update stats.
            self.stat_space_allocated
                .set(self.stat_space_allocated.get() + size);

            // Return the allocation.
            unsafe { cur_block.as_mut_ptr().add(start_index_proposal) }
        }
        // If it won't fit in the current block, create a new block and use that.
        else {
            // Calculate the size that the next shared block should be.
            // This is where we implement progressive block growth.  We do the
            // growth as a factor of the total arena capacity, not just the
            // current block.
            let next_shared_size = match self.growth_strategy {
                GrowthStrategy::Constant => self.min_block_size,
                GrowthStrategy::Percentage(perc) => {
                    let a = self.stat_space_occupied.get() / 100 * perc as usize;
                    let b = a % self.min_block_size;
                    self.min_block_size.max(a - b)
                }
            };

            // We take the minimum of the over-all arena waste percentage and
            // the current block's waste percentage because if the current
            // block is below the threshhold, then we can start a new block
            // without cumulatively increasing the waste percentage of the
            // whole arena.
            let waste_percentage = {
                let block = blocks.front().unwrap();
                let w1 = ((block.capacity() - block.len()) * 100) / block.capacity();
                let w2 = ((self.stat_space_occupied.get() - self.stat_space_allocated.get()) * 100)
                    / self.stat_space_occupied.get();
                w1.min(w2)
            };

            // Are we making a new shared block, or a one-off for this
            // allocation?
            let is_shared_block = (size + alignment) <= next_shared_size
                && waste_percentage <= self.max_waste_percentage;

            // Determine the size of the new block.
            let new_block_size = if is_shared_block {
                next_shared_size
            } else {
                size + alignment - 1
            };

            // Update stats.
            self.stat_space_occupied
                .set(self.stat_space_occupied.get() + new_block_size);
            self.stat_space_allocated
                .set(self.stat_space_allocated.get() + size);

            // Add the new block.

            // Get the new block.
            let new_block = {
                if is_shared_block {
                    // If it's shared, add to the front,
                    blocks.push_front(Vec::with_capacity(new_block_size));
                    blocks.front_mut().unwrap()
                } else {
                    // Otherwise add to the the back.
                    blocks.push_back(Vec::with_capacity(new_block_size));
                    blocks.back_mut().unwrap()
                }
            };

            // Do the bump allocation.
            let start_index = alignment_offset(new_block.as_ptr() as usize, alignment);
            unsafe { new_block.set_len(start_index + size) };

            // Return the allocation.
            unsafe { new_block.as_mut_ptr().add(start_index) }
        }
    }

    //------------------------------------------------------------------------
    // Misc methods.

    /// Frees all memory currently allocated by the arena.
    pub fn clear(&mut self) {
        unsafe { self.clear_unchecked() }
    }

    /// Unsafe version of `clear()`, without any safetey checks.
    ///
    /// # Safety
    ///
    /// This method is _extremely_ unsafe. It can easily create dangling
    /// references to invalid memory.  Only use this if (a) you can't use the
    /// safe version for some reason and (b) you really know what you're doing.
    ///
    /// The safe version of this method takes a mutable reference to `self`,
    /// which ensures at compile time that there are no other references to
    /// either the arena itself or its allocations.
    ///
    /// This method, on the other hand, makes no such guarantees.  It will
    /// quite happily free all of its memory even with hundreds or thousands
    /// of outstanding references pointing to it.
    pub unsafe fn clear_unchecked(&self) {
        let mut blocks = self.blocks.borrow_mut();

        blocks.clear();

        self.stat_space_occupied.set(0);
        self.stat_space_allocated.set(0);
    }

    // /// Returns statistics about the current usage as a tuple:
    // /// (space occupied, space allocated, block count, large block count)
    // ///
    // /// Space occupied is the amount of real memory that the Arena
    // /// is taking up (not counting book keeping).
    // ///
    // /// Space allocated is the amount of occupied space that is
    // /// actually used.  In other words, it is the sum of the all the
    // /// allocation requests made to the arena by client code.
    // ///
    // /// Block count is the number of blocks that have been allocated.
    // pub fn stats(&self) -> (usize, usize, usize) {
    //     let occupied = self.stat_space_occupied.get();
    //     let allocated = self.stat_space_allocated.get();
    //     let blocks = self.blocks.borrow().len();

    //     (occupied, allocated, blocks)
    // }
}

/// Strategy for determining the size of new blocks.
///
/// - `Constant`: no growth.  All blocks are the same size.
/// - `Percentage`: block size is determined as a percentage of the current
///                 total arena size, with the configured block size as a
///                 minimum.  Recommended values are between 10 and 50 percent.
///
/// For most use-cases `Constant` is recommended.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum GrowthStrategy {
    Constant,
    Percentage(u8),
}

impl Default for GrowthStrategy {
    fn default() -> GrowthStrategy {
        GrowthStrategy::Constant
    }
}
