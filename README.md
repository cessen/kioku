# Kioku

A growable memory arena for Copy types.

This arena works by internally allocating memory in large-ish blocks of
memory one-at-a-time, and doling out memory from the current block in
linear order until its space runs out.

Additionally, it attempts to minimize wasted space through some heuristics,
and has special handling for larger-than-block-size allocation requests.

Some contrived example usage:

```rust
let arena = Arena::new().with_block_size(1024);

let integer = arena.alloc(42);
let array1 = arena.copy_slice(&[1, 2, 3, 4, 5, 42]);
assert_eq!(*integer, array1[5]);

*integer = 16;
array1[1] = 16;
assert_eq!(*integer, array1[1]);

let character = arena.alloc('A');
let array2 = arena.alloc_array('A', 42);
assert_eq!(array2.len(), 42);
assert_eq!(*character, array2[20]);

*character = '学';
array2[30] = '学';
assert_eq!(*character, array2[30]);
```

Other features include:

* Allocating with specific memory alignment.
* Allocating strings.
* Configurable growth strategies.


## License

This project is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

at your option.

## Contributing

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in Kioku by you will be licensed as above, without any additional
terms or conditions.