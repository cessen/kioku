use kioku::Arena;

#[test]
fn alloc() {
    let arena = Arena::new();
    let a = arena.alloc('A');
    assert_eq!('A', *a);
}

#[test]
fn alloc_array_01() {
    let arena = Arena::new();
    let a = arena.alloc_array('A', 3);
    let b = arena.alloc_array('A', 0);
    assert_eq!(&['A', 'A', 'A'], a);
    assert_eq!(&['B'; 0][..], b);
}

#[test]
fn copy_slice_01() {
    let arena = Arena::new();
    let a = arena.copy_slice(&['A', 'B', 'C']);
    let b = arena.copy_slice::<char>(&[]);
    assert_eq!(&['A', 'B', 'C'], a);
    assert_eq!(&['D'; 0][..], b);
}

#[test]
fn copy_str_01() {
    let arena = Arena::new();
    let a = arena.copy_str("Hello there! こんにちは！");
    let b = arena.copy_str("");
    assert_eq!("Hello there! こんにちは！", a);
    assert_eq!("", b);
}

#[test]
fn alloc_align_01() {
    let arena = Arena::new();
    let a = arena.alloc_align('A', 64);
    let b = arena.alloc_align('B', 64);
    assert_eq!('A', *a);
    assert_eq!('B', *b);
    assert_eq!(0, a as *const _ as usize % 64);
    assert_eq!(0, b as *const _ as usize % 64);
}

#[test]
fn alloc_array_align_01() {
    let arena = Arena::new();
    let a = arena.alloc_array_align('A', 3, 64);
    let b = arena.alloc_array_align('B', 3, 64);
    let c = arena.alloc_array_align('C', 0, 64);
    assert_eq!(&['A', 'A', 'A'], a);
    assert_eq!(&['B', 'B', 'B'], b);
    assert_eq!(&['C'; 0][..], c);
    assert_eq!(0, &a[0] as *const _ as usize % 64);
    assert_eq!(0, &b[0] as *const _ as usize % 64);
}

#[test]
fn copy_slice_align_01() {
    let arena = Arena::new();
    let a = arena.copy_slice_align(&['A', 'B', 'C'], 64);
    let b = arena.copy_slice_align(&['D', 'E', 'F'], 64);
    let c = arena.copy_slice_align::<char>(&[], 64);
    assert_eq!(&['A', 'B', 'C'], a);
    assert_eq!(&['D', 'E', 'F'], b);
    assert_eq!(&['G'; 0][..], c);
    assert_eq!(0, &a[0] as *const _ as usize % 64);
    assert_eq!(0, &b[0] as *const _ as usize % 64);
}

#[test]
fn alloc_uninit_01() {
    let arena = Arena::new();
    let _a = arena.alloc_uninit::<char>();
    let _b = arena.alloc_uninit::<char>();
}

#[test]
fn alloc_array_uninit_01() {
    let arena = Arena::new();
    let a = arena.alloc_array_uninit::<char>(3);
    let b = arena.alloc_array_uninit::<char>(0);
    assert_eq!(3, a.len());
    assert_eq!(0, b.len());
}

#[test]
fn alloc_align_uninit_01() {
    let arena = Arena::new();
    let a = arena.alloc_align_uninit::<char>(64);
    let b = arena.alloc_align_uninit::<char>(64);
    assert_eq!(0, a as *const _ as usize % 64);
    assert_eq!(0, b as *const _ as usize % 64);
}

#[test]
fn alloc_array_align_uninit_01() {
    let arena = Arena::new();
    let a = arena.alloc_array_align_uninit::<char>(3, 64);
    let b = arena.alloc_array_align_uninit::<char>(3, 64);
    let c = arena.alloc_array_align_uninit::<char>(0, 64);
    assert_eq!(3, a.len());
    assert_eq!(3, b.len());
    assert_eq!(0, c.len());
    assert_eq!(0, &a[0] as *const _ as usize % 64);
    assert_eq!(0, &b[0] as *const _ as usize % 64);
}

#[test]
fn lots_of_allocs_01() {
    // To force multiple blocks.
    let arena = Arena::new().with_block_size(64);

    for _ in 0..512 {
        let a = arena.alloc('A');
        assert_eq!('A', *a);
    }
}

#[test]
fn big_alloc_01() {
    // To make sure larger-than-block-size allocations succeed.
    let arena = Arena::new().with_block_size(64);
    let a = arena.alloc('A');
    let b = arena.alloc('B');
    let c = arena.alloc_array(['C'; 8], 32);
    let d = arena.alloc('D');
    let e = arena.alloc('E');

    assert_eq!('A', *a);
    assert_eq!('B', *b);
    assert_eq!(&[['C'; 8]; 32], c);
    assert_eq!('D', *d);
    assert_eq!('E', *e);
}

//-----------------------------------------------------------
// Tests to make sure malformed alignments are rejected.

#[test]
#[should_panic]
fn alloc_align_malformed_01() {
    Arena::new().alloc_align('A', 6);
}

#[test]
#[should_panic]
fn alloc_align_malformed_02() {
    Arena::new().alloc_align('A', 0);
}

//-----------------------------------------------------------
// Tests to make sure zero-sized types are rejected.

#[test]
#[should_panic]
fn zero_sized_types_01() {
    Arena::new().alloc(());
}

#[test]
#[should_panic]
fn zero_sized_types_02() {
    Arena::new().alloc_array((), 0);
}

#[test]
#[should_panic]
fn zero_sized_types_03() {
    Arena::new().copy_slice(&[()]);
}

#[test]
#[should_panic]
fn zero_sized_types_04() {
    Arena::new().alloc_align((), 4);
}

#[test]
#[should_panic]
fn zero_sized_types_05() {
    Arena::new().alloc_array_align((), 0, 4);
}

#[test]
#[should_panic]
fn zero_sized_types_06() {
    Arena::new().copy_slice_align(&[()], 4);
}

#[test]
#[should_panic]
fn zero_sized_types_07() {
    Arena::new().alloc_uninit::<()>();
}

#[test]
#[should_panic]
fn zero_sized_types_08() {
    Arena::new().alloc_array_uninit::<()>(0);
}

#[test]
#[should_panic]
fn zero_sized_types_09() {
    Arena::new().alloc_align_uninit::<()>(4);
}

#[test]
#[should_panic]
fn zero_sized_types_10() {
    Arena::new().alloc_array_align_uninit::<()>(0, 4);
}
