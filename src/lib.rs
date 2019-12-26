//! Kioku is a growable memory arena/pool

#![allow(clippy::redundant_field_names)]
#![allow(clippy::needless_return)]
#![allow(clippy::mut_from_ref)]
#![allow(clippy::transmute_ptr_to_ptr)]

mod utils;

use std::{
    alloc::Layout,
    cell::{Cell, RefCell},
    fmt,
    mem::{align_of, size_of, transmute, MaybeUninit},
    slice,
};

use utils::{alignment_offset, min_alignment, repeat_layout};

const GROWTH_FRACTION: usize = 8; // 1/N  (smaller number leads to bigger allocations)
const DEFAULT_INITIAL_BLOCK_SIZE: usize = 1 << 10; // 1 KiB
const DEFAULT_MAX_WASTE_PERCENTAGE: usize = 10;

/// A growable memory arena for Copy types.
///
/// The arena works by allocating memory in blocks of slowly increasing size.
/// It doles out memory from the current block until an amount of memory is
/// requested that doesn't fit in the remainder of the current block, and then
/// allocates a new block.
///
/// Additionally, it attempts to minimize wasted space through some heuristics.
/// By default, it tries to keep memory waste within the arena below 10%.
///
/// # Custom Memory Alignment
///
/// All methods with a custom alignment parameter require the alignment to be
/// greater than zero and a power of two.  Moreover, the alignment parameter
/// can only increase the strictness of the alignment, and will be ignored if
/// less strict than the natural alignment of the type being allocated.
///
/// Array allocation methods with alignment parameters only align the head of
/// the array to that alignment, and otherwise follow standard array memory
/// layout.
///
/// # Zero Sized Types
///
/// Zero-sized types such as `()` are unsupported.  All allocations will panic
/// if `T` is zero-sized.
///
/// However, you *can* allocate zero length arrays using the array allocation
/// methods.  Only `T` itself must be non-zero-sized.
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
    /// Create a new arena with default settings.
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
    /// Space allocated is the amount of occupied space that is
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

    /// Frees all memory currently allocated by the arena.
    pub fn clear(&mut self) {
        unsafe { self.clear_unchecked() }
    }

    /// Unsafe version of `clear()`, without any safetey checks.
    ///
    /// This method is _extremely_ unsafe. It can easily create dangling
    /// references to invalid memory.  Only use this if (a) you can't use the
    /// safe version for some reason and (b) you really know what you're doing.
    ///
    /// The safe version of this method (`clear()`) takes a mutable reference
    /// to `self`, which ensures at compile time that there are no other
    /// references to either the arena itself or its allocations.
    ///
    /// This method, on the other hand, makes no such guarantees.  It will
    /// quite happily free all of its memory even with hundreds or thousands
    /// of outstanding references pointing to it.
    pub unsafe fn clear_unchecked(&self) {
        let mut blocks = self.blocks.borrow_mut();

        blocks.clear();
        blocks.shrink_to_fit();
        blocks.push(Vec::with_capacity(self.min_block_size));

        self.stat_space_occupied.set(self.min_block_size);
        self.stat_space_allocated.set(0);
    }

    //------------------------------------------------------------------------
    // Basic methods

    /// Allocates a `T` initialized to `value`
    #[inline]
    pub fn item<T: Copy>(&self, value: T) -> &mut T {
        let memory = self.item_uninit();
        unsafe {
            *memory.as_mut_ptr() = value;
        }
        unsafe { transmute(memory) }
    }

    /// Allocates a `[T]` with all elements initialized to `value`.
    #[inline]
    pub fn array<T: Copy>(&self, value: T, len: usize) -> &mut [T] {
        let memory = self.array_uninit(len);

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
        let memory = self.array_uninit(slice.len());

        for (v, slice) in memory.iter_mut().zip(slice.iter()) {
            unsafe {
                *v.as_mut_ptr() = *slice;
            }
        }

        unsafe { transmute(memory) }
    }

    //------------------------------------------------------------------------
    // Initialized allocation methods with alignment.

    /// Allocates a `T` initialized to `value`, aligned to at least `align`
    /// bytes.
    #[inline]
    pub fn item_align<T: Copy>(&self, value: T, align: usize) -> &mut T {
        let memory = self.item_align_uninit(align);
        unsafe {
            *memory.as_mut_ptr() = value;
        }
        unsafe { transmute(memory) }
    }

    /// Allocates a `[T]` with all elements initialized to `value`, aligned to
    /// at least `align` bytes.
    #[inline]
    pub fn array_align<T: Copy>(&self, value: T, len: usize, align: usize) -> &mut [T] {
        let memory = self.array_align_uninit(len, align);

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
    pub fn copy_slice_align<T: Copy>(&self, other: &[T], align: usize) -> &mut [T] {
        let memory = self.array_align_uninit(other.len(), align);

        for (v, other) in memory.iter_mut().zip(other.iter()) {
            unsafe {
                *v.as_mut_ptr() = *other;
            }
        }

        unsafe { transmute(memory) }
    }

    //------------------------------------------------------------------------
    // Uninitialized allocation methods.

    /// Allocates an uninitialized `T`.
    #[inline]
    pub fn item_uninit<T: Copy>(&self) -> &mut MaybeUninit<T> {
        assert!(
            size_of::<T>() > 0,
            "`Arena` does not support zero-sized types."
        );

        let memory = self.alloc_raw(&Layout::new::<T>()) as *mut MaybeUninit<T>;

        unsafe { memory.as_mut().unwrap() }
    }

    /// Allocates a uninitialized `[T]`.
    #[inline]
    pub fn array_uninit<T: Copy>(&self, len: usize) -> &mut [MaybeUninit<T>] {
        self.array_align_uninit(len, align_of::<T>())
    }

    /// Allocates an uninitialized `T`, aligned to at least `align` bytes.
    #[inline]
    pub fn item_align_uninit<T: Copy>(&self, align: usize) -> &mut MaybeUninit<T> {
        assert!(
            size_of::<T>() > 0,
            "`Arena` does not support zero-sized types."
        );

        let layout = min_alignment(&Layout::new::<T>(), align);
        let memory = self.alloc_raw(&layout) as *mut MaybeUninit<T>;
        unsafe { memory.as_mut().unwrap() }
    }

    /// Allocates a uninitialized `[T]`, aligned to at least `align` bytes.
    #[inline]
    pub fn array_align_uninit<T: Copy>(&self, len: usize, align: usize) -> &mut [MaybeUninit<T>] {
        assert!(
            size_of::<T>() > 0,
            "`Arena` does not support zero-sized types."
        );

        let layout = min_alignment(&repeat_layout(&Layout::new::<T>(), len), align);
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
