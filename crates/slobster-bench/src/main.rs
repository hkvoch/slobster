use slobster::slab::SlabAllocator;

fn main() {
	let alloc = SlabAllocator::<[u8; 256]>::new(None).unwrap();
	let mut keys = Vec::with_capacity(1000);

	for i in 1..=2 {
		eprintln!("{i}/2");

		for _ in 0..1000 {
			keys.push(alloc.alloc());
		}

		for &k in &keys {
			unsafe {
				alloc.free_unchecked(k);
			}
		}

		keys.clear();
	}
}
