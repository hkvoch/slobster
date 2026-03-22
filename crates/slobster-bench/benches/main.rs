use core::fmt;
use std::alloc::{GlobalAlloc, Layout};
use std::hint::black_box;
use std::ptr::NonNull;
use std::time::Duration;

use criterion::measurement::Measurement;
use criterion::{BatchSize, BenchmarkGroup, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rand::seq::SliceRandom;

trait SlabAllocator<T> {
	type Key: Copy;

	fn alloc(&mut self, value: T) -> Self::Key;
	fn free(&mut self, key: Self::Key);
	fn name(&self) -> impl fmt::Display;
	fn dangling() -> Self::Key;
}

struct SysSlabAllocator;

impl<T> SlabAllocator<T> for SysSlabAllocator {
	type Key = NonNull<T>;

	fn dangling() -> Self::Key { NonNull::dangling() }

	fn alloc(&mut self, value: T) -> Self::Key {
		const { assert!(size_of::<T>() > 0) };

		unsafe {
			let ptr = NonNull::new(std::alloc::System.alloc(Layout::new::<T>()))
				.unwrap()
				.cast::<T>();
			ptr.write(value);
			ptr
		}
	}

	fn free(&mut self, key: Self::Key) {
		unsafe {
			std::alloc::System.dealloc(key.as_ptr().cast(), Layout::new::<T>());
		}
	}

	fn name(&self) -> impl fmt::Display { "system" }
}

struct SlabSlabAllocator<T> {
	inner: slab::Slab<T>,
}

impl<T> SlabSlabAllocator<T> {
	fn new() -> Self {
		Self {
			inner: slab::Slab::new(),
		}
	}
}

impl<T> SlabAllocator<T> for SlabSlabAllocator<T> {
	type Key = usize;

	fn dangling() -> Self::Key { 0 }

	fn alloc(&mut self, value: T) -> Self::Key { self.inner.insert(value) }

	fn free(&mut self, key: Self::Key) { self.inner.remove(key); }

	fn name(&self) -> impl fmt::Display { "tokio-rs-slab" }
}

struct SlabbinSlabAllocator<T> {
	inner: slabbin::SlabAllocator<T>,
	slab_capacity: usize,
}

impl<T> SlabbinSlabAllocator<T> {
	fn new(slab_capacity: usize) -> Self {
		Self {
			inner: slabbin::SlabAllocator::new(slab_capacity),
			slab_capacity,
		}
	}
}

impl<T> SlabAllocator<T> for SlabbinSlabAllocator<T> {
	type Key = NonNull<T>;

	fn dangling() -> Self::Key { NonNull::dangling() }

	fn alloc(&mut self, value: T) -> Self::Key {
		let ptr = self.inner.allocate();
		unsafe { ptr.write(value) };
		ptr
	}

	fn free(&mut self, key: Self::Key) { unsafe { self.inner.deallocate(key) } }

	fn name(&self) -> impl fmt::Display { format!("slabbin-{}", self.slab_capacity) }
}

impl<T> SlabAllocator<T> for slobster::slab::SlabAllocator<T> {
	type Key = NonNull<T>;

	fn dangling() -> Self::Key { NonNull::dangling() }

	fn alloc(&mut self, value: T) -> Self::Key {
		let ptr = slobster::slab::SlabAllocator::<T>::alloc(self);
		unsafe { ptr.write(value) };
		ptr
	}

	fn free(&mut self, key: Self::Key) {
		unsafe {
			self.free_unchecked(key);
		}
	}

	fn name(&self) -> impl fmt::Display { "slobster" }
}

criterion_group!(benches, bench_roundtrip, bench_spike, bench_randomised);
criterion_main!(benches);

fn bench_roundtrip(c: &mut Criterion) {
	let mut group = c.benchmark_group("roundtrip");
	group.measurement_time(Duration::from_secs(10));

	let n_iter = 1_000_usize;

	macro_rules! sizes {
		($($size:literal),* $(,)?) => {{
			$(bench_roundtrip_size::<$size, _>(&mut group, n_iter);)*
		}};
	}

	// sizes!(32);
	sizes!(1, 2, 3, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096);
	group.finish();
}

fn bench_roundtrip_size<const SIZE: usize, M>(group: &mut BenchmarkGroup<'_, M>, n_iter: usize)
where M: Measurement {
	bench_one_roundtrip::<SIZE, _, _>(SlabSlabAllocator::new(), group, n_iter);
	bench_one_roundtrip::<SIZE, _, _>(SlabbinSlabAllocator::new(128), group, n_iter);
	bench_one_roundtrip::<SIZE, _, _>(SlabbinSlabAllocator::new(512), group, n_iter);
	bench_one_roundtrip::<SIZE, _, _>(SlabbinSlabAllocator::new(1024), group, n_iter);
	bench_one_roundtrip::<SIZE, _, _>(
		slobster::slab::SlabAllocator::new(slobster::slab::SlabAllocatorOptions {
			pages_per_slab: core::num::NonZeroUsize::new(256).unwrap(),
		})
		.unwrap(),
		group,
		n_iter,
	);
	bench_one_roundtrip::<SIZE, _, _>(SysSlabAllocator, group, n_iter);
}

fn bench_one_roundtrip<const SIZE: usize, A, M>(mut allocator: A, group: &mut BenchmarkGroup<'_, M>, n_iter: usize)
where
	A: SlabAllocator<[u8; SIZE]>,
	M: Measurement,
{
	group.throughput(Throughput::Bytes((SIZE * n_iter) as u64));
	group.bench_with_input(
		BenchmarkId::from_parameter(format!("{}-{}", allocator.name(), SIZE)),
		&SIZE,
		|b, _| {
			b.iter(|| {
				bench_roundtrip_do(&mut allocator, black_box(n_iter));
			});
		},
	);
}

fn bench_roundtrip_do<const SIZE: usize, A>(allocator: &mut A, n_iter: usize)
where A: SlabAllocator<[u8; SIZE]> {
	for _ in 0..n_iter {
		let key = black_box(allocator.alloc([0; SIZE]));
		allocator.free(black_box(key));
	}
}

fn bench_spike(c: &mut Criterion) {
	let mut group = c.benchmark_group("spike");
	group.measurement_time(Duration::from_secs(10));

	let n_iter = 1_000_usize;

	macro_rules! sizes {
		($($size:literal),* $(,)?) => {{
			$(bench_spike_size::<$size, _>(&mut group, n_iter);)*
		}};
	}

	// sizes!(256);
	sizes!(1, 2, 3, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096);
	group.finish();
}

fn bench_spike_size<const SIZE: usize, M>(group: &mut BenchmarkGroup<'_, M>, n_iter: usize)
where M: Measurement {
	bench_one_spike::<SIZE, _, _>(SlabSlabAllocator::new(), group, n_iter);
	bench_one_spike::<SIZE, _, _>(SlabbinSlabAllocator::new(128), group, n_iter);
	bench_one_spike::<SIZE, _, _>(SlabbinSlabAllocator::new(512), group, n_iter);
	bench_one_spike::<SIZE, _, _>(SlabbinSlabAllocator::new(1024), group, n_iter);
	bench_one_spike::<SIZE, _, _>(slobster::slab::SlabAllocator::new(None).unwrap(), group, n_iter);
	bench_one_spike::<SIZE, _, _>(SysSlabAllocator, group, n_iter);
}

fn bench_one_spike<const SIZE: usize, A, M>(mut allocator: A, group: &mut BenchmarkGroup<'_, M>, n_iter: usize)
where
	A: SlabAllocator<[u8; SIZE]>,
	M: Measurement,
{
	group.throughput(Throughput::Bytes((SIZE * n_iter) as u64));
	let mut scratch = vec![A::dangling(); n_iter];
	bench_spike_do(&mut allocator, black_box(&mut scratch));
	group.bench_function(
		BenchmarkId::from_parameter(format!("{}-{}", allocator.name(), SIZE)),
		|b| {
			b.iter(
				// || vec![A::dangling(); n_iter],
				|| {
					bench_spike_do(&mut allocator, black_box(&mut scratch));
				},
				// BatchSize::SmallInput,
			);
		},
	);
}

fn bench_spike_do<const SIZE: usize, A>(allocator: &mut A, scratch: &mut [A::Key])
where A: SlabAllocator<[u8; SIZE]> {
	for sc in scratch.iter_mut() {
		*sc = black_box(allocator.alloc([0; SIZE]));
	}

	for sc in scratch.iter_mut() {
		allocator.free(black_box(*sc));
	}
}

fn bench_randomised(c: &mut Criterion) {
	let mut group = c.benchmark_group("randomised");
	group.measurement_time(Duration::from_secs(10));

	let n_iter = 1_000_usize;

	macro_rules! sizes {
		($($size:literal),* $(,)?) => {{
			$(bench_randomised_size::<$size, _>(&mut group, n_iter);)*
		}};
	}

	// sizes!(256);
	sizes!(1, 2, 3, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096);
	group.finish();
}

fn bench_randomised_size<const SIZE: usize, M>(group: &mut BenchmarkGroup<'_, M>, n_iter: usize)
where M: Measurement {
	bench_one_randomised::<SIZE, _, _>(SlabSlabAllocator::new(), group, n_iter);
	bench_one_randomised::<SIZE, _, _>(SlabbinSlabAllocator::new(128), group, n_iter);
	bench_one_randomised::<SIZE, _, _>(SlabbinSlabAllocator::new(512), group, n_iter);
	bench_one_randomised::<SIZE, _, _>(SlabbinSlabAllocator::new(1024), group, n_iter);
	bench_one_randomised::<SIZE, _, _>(slobster::slab::SlabAllocator::new(None).unwrap(), group, n_iter);
	bench_one_randomised::<SIZE, _, _>(SysSlabAllocator, group, n_iter);
}

fn bench_one_randomised<const SIZE: usize, A, M>(mut allocator: A, group: &mut BenchmarkGroup<'_, M>, n_iter: usize)
where
	A: SlabAllocator<[u8; SIZE]>,
	M: Measurement,
{
	group.throughput(Throughput::Bytes((SIZE * n_iter) as u64));
	let mut scratch: Vec<_> = std::iter::repeat_n(A::dangling(), n_iter).enumerate().collect();
	scratch.shuffle(&mut rand::rng());

	bench_randomised_do(&mut allocator, black_box(&mut scratch));
	group.bench_function(
		BenchmarkId::from_parameter(format!("{}-{}", allocator.name(), SIZE)),
		|b| {
			b.iter(
				// || vec![A::dangling(); n_iter],
				|| {
					bench_randomised_do(&mut allocator, black_box(&mut scratch));
				},
				// BatchSize::SmallInput,
			);
		},
	);
}

fn bench_randomised_do<const SIZE: usize, A>(allocator: &mut A, scratch: &mut [(usize, A::Key)])
where A: SlabAllocator<[u8; SIZE]> {
	for (_, sc) in scratch.iter_mut() {
		*sc = black_box(allocator.alloc([0; SIZE]));
	}

	for &(ix, _) in scratch.iter() {
		allocator.free(black_box(scratch[ix].1));
	}
}
