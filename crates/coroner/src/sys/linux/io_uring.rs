use core::cell::Cell;
use core::ptr::{NonNull, addr_of_mut};
use core::sync::atomic::{AtomicU32, Ordering};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

use raw::{IoUringFeatures, IoUringParams, IoUringSetupFlags, io_uring_setup};

use crate::sys::unix::Mmap;
use crate::sys::{MmapFlags, MmapProtect, SysError, SysResult};
use crate::utils::{for_each_field, size_of_field};

mod raw {
	use std::os::fd::{FromRawFd, OwnedFd, RawFd};

	use bitflags::bitflags;

	use crate::sys::{SysError, SysResult};

	#[derive(Debug, Default)]
	#[repr(C)]
	pub(super) struct IoRingSqOffsets {
		pub(super) head: u32,
		pub(super) tail: u32,
		pub(super) ring_mask: u32,
		pub(super) ring_entries: u32,
		pub(super) flags: u32,
		pub(super) dropped: u32,
		pub(super) array: u32,
		pub(super) resv1: u32,
		pub(super) user_addr: u64,
	}

	#[derive(Debug, Default)]
	#[repr(C)]
	pub(super) struct IoRingCqOffsets {
		pub(super) head: u32,
		pub(super) tail: u32,
		pub(super) ring_mask: u32,
		pub(super) ring_entries: u32,
		pub(super) overflow: u32,
		pub(super) cqes: u32,
		pub(super) flags: u32,
		pub(super) resv1: u32,
		pub(super) user_addr: u64,
	}

	#[derive(Debug, Default)]
	#[repr(C)]
	pub(super) struct IoUringParams {
		pub(super) sq_entries: u32,
		pub(super) cq_entries: u32,
		pub(super) flags: IoUringSetupFlags,
		pub(super) sq_thread_cpu: u32,
		pub(super) sq_thread_idle: u32,
		pub(super) features: IoUringFeatures,
		pub(super) wq_fd: u32,
		pub(super) resv: [u32; 3],
		pub(super) sq_off: IoRingSqOffsets,
		pub(super) cq_off: IoRingCqOffsets,
	}

	bitflags! {
		#[derive(Debug, Default, Clone, Copy)]
		#[repr(transparent)]
		pub(super) struct IoUringSetupFlags: u32 {
			const IOPOLL = (1 << 0);
			const SQPOLL = (1 << 1);
			const SQ_AFF = (1 << 2);
			const CQSIZE = (1 << 3);
			const CLAMP = (1 << 4);
			const ATTACH_WQ = (1 << 5);
			const R_DISABLED = (1 << 6);
			const SUBMIT_ALL = (1 << 7);
			const COOP_TASKRUN = (1 << 8);
			const TASKRUN_FLAG = (1 << 9);
			const SQE128 = (1 << 10);
			const CQE32 = (1 << 11);
			const SINGLE_ISSUER = (1 << 12);
			const DEFER_TASKRUN = (1 << 13);
			const NO_MMAP = (1 << 14);
			const REGISTERED_FD_ONLY = (1 << 15);
			const NO_SQARRAY = (1 << 16);
			const HYBRID_IOPOLL = (1 << 17);
			const CQE_MIXED = (1 << 18);
			const SQE_MIXED = (1 << 19);
			const SQ_REWIND = (1 << 20);
		}
	}

	bitflags! {
		#[derive(Debug, Default, Clone, Copy)]
		#[repr(transparent)]
		pub(super) struct IoUringFeatures: u32 {
			const SINGLE_MMAP = 1 << 0;
			const NODROP = 1 << 1;
			const SUBMIT_STABLE = 1 << 2;
			const RW_CUR_POS = 1 << 3;
			const CUR_PERSONALITY = 1 << 4;
			const FAST_POLL = 1 << 5;
			const POLL_32BITS = 1 << 6;
			const SQPOLL_NONFIXED = 1 << 7;
			const EXT_ARG = 1 << 8;
			const NATIVE_WORKERS = 1 << 9;
			const RSRC_TAGS = 1 << 10;
			const CQE_SKIP = 1 << 11;
			const LINKED_FILE = 1 << 12;
			const REG_REG_RING = 1 << 13;
			const RECVSEND_BUNDLE = 1 << 14;
			const MIN_TIMEOUT = 1 << 15;
			const RW_ATTR = 1 << 16;
			const NO_IOWAIT = 1 << 17;
		}
	}

	pub(super) fn io_uring_setup(entries: u32, params: &mut IoUringParams) -> SysResult<OwnedFd> {
		let ret = unsafe { libc::syscall(libc::SYS_io_uring_setup, entries, params as *mut IoUringParams) };
		if ret >= 0
			&& let Ok(ring_fd) = RawFd::try_from(ret)
		{
			Ok(unsafe { OwnedFd::from_raw_fd(ring_fd) })
		} else {
			Err(SysError::from_syscall_error(ret))
		}
	}
}

pub(crate) struct IoUringOptions {
	pub sq_entries: u32,
}

struct SubmissionQueue {
	head: NonNull<AtomicU32>,
	tail: NonNull<AtomicU32>,
	flags: NonNull<AtomicU32>,
	dropped: NonNull<AtomicU32>,
	mask: u32,
	entries: u32,
	sqes: NonNull<[SqEntry]>,
	sqpoll: bool,
	sqe_tail: Cell<u32>,
}

impl SubmissionQueue {
	fn get_sqe(&self) -> Option<&SqEntry> {
		let head = self.load_head();
		let tail = self.sqe_tail.get();

		if tail.wrapping_sub(head) >= self.entries {
			return None;
		}

		let sqe = &self.load_sqes()[(tail & self.mask) as usize];
		self.sqe_tail.set(tail + 1);

		sqe.zeroize();
		Some(sqe)
	}

	fn load_head(&self) -> u32 {
		unsafe {
			if self.sqpoll {
				self.head.as_ref().load(Ordering::Acquire)
			} else {
				self.head.as_ref().load(Ordering::Relaxed)
			}
		}
	}

	fn load_tail(&self) -> u32 { unsafe { self.tail.as_ref().load(Ordering::Relaxed) } }

	fn load_sqes(&self) -> &[SqEntry] { unsafe { self.sqes.as_ref() } }
}

#[repr(C)]
struct SqEntry {
	opcode: Cell<u8>,
	flags: Cell<u8>,
	ioprio: Cell<u16>,
	fd: Cell<i32>,
	off: Cell<SqEntryOff>,
	addr: Cell<SqEntryAddr>,
	len: Cell<u32>,
	op_flags: Cell<SqEntryOpFlags>,
	user_data: Cell<u64>,
	buf: Cell<SqEntryBuf>,
	personality: Cell<u16>,
	file_idx: Cell<SqEntryFileIdx>,
	fin: Cell<SqEntryFinal>,
}

impl SqEntry {
	fn zeroize(&self) {
		for_each_field!(@cell_set self, SqEntry,
			opcode = 0,
			flags = 0,
			ioprio = 0,
			fd = 0,
			off = SqEntryOff {x_full: [0; 8]},
			addr = SqEntryAddr {x_full: [0; 8]},
			len = 0,
			op_flags = SqEntryOpFlags {x_full: [0; 4]},
			user_data = 0,
			buf = SqEntryBuf {x_full: [0; 2]},
			personality = 0,
			file_idx = SqEntryFileIdx {x_full: [0; 4]},
			fin = SqEntryFinal {x_full: [0; 16]},
		);
	}
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct SqEntryCmdOp {
	op: u32,
	pad: u32,
}

#[repr(C)]
union SqEntryOff {
	off: u64,
	addr2: u64,
	cmd_op: SqEntryCmdOp,
	x_full: [u8; 8],
}

const _: () = {
	assert!(size_of::<SqEntryOff>() == size_of_field!(SqEntryOff, x_full));
};

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct SqEntryLevelOpt {
	level: u32,
	optname: u32,
}

#[repr(C)]
union SqEntryAddr {
	addr: u64,
	splice_off_in: u64,
	level_opt: SqEntryLevelOpt,
	x_full: [u8; 8],
}

const _: () = {
	assert!(size_of::<SqEntryAddr>() == size_of_field!(SqEntryAddr, x_full));
};

type kernel_rwf_t = u32;

#[repr(C)]
union SqEntryOpFlags {
	rw_flags: kernel_rwf_t,
	fsync_flags: u32,
	poll_events: u16,
	poll32_events: u32,
	sync_range_flags: u32,
	msg_flags: u32,
	timeout_flags: u32,
	accept_flags: u32,
	cancel_flags: u32,
	open_flags: u32,
	statx_flags: u32,
	fadvise_advice: u32,
	splice_flags: u32,
	rename_flags: u32,
	unlink_flags: u32,
	hardlink_flags: u32,
	xattr_flags: u32,
	msg_ring_flags: u32,
	uring_cmd_flags: u32,
	waitid_flags: u32,
	futex_flags: u32,
	install_fd_flags: u32,
	nop_flags: u32,
	pipe_flags: u32,
	x_full: [u8; 4],
}

const _: () = {
	assert!(size_of::<SqEntryOpFlags>() == size_of_field!(SqEntryOpFlags, x_full));
};

#[repr(C, packed)]
union SqEntryBuf {
	buf_index: u16,
	buf_group: u16,
	x_full: [u8; 2],
}

const _: () = {
	assert!(size_of::<SqEntryBuf>() == size_of_field!(SqEntryBuf, x_full));
};

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct SqEntryAddrLen {
	addr_len: u16,
	pad: u16,
}

#[repr(C)]
union SqEntryFileIdx {
	splice_fd_in: u32,
	file_index: u32,
	zcrx_ifq_idx: u32,
	optlen: u32,
	addr_len: SqEntryAddrLen,
	x_full: [u8; 4],
}

const _: () = {
	assert!(size_of::<SqEntryFileIdx>() == size_of_field!(SqEntryFileIdx, x_full));
};

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct SqEntryAddr3 {
	addr3: u64,
	pad: u64,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct SqEntryAttr {
	attr_ptr: u64,
	attr_type_mask: u64,
}

#[repr(C)]
union SqEntryFinal {
	addr3: SqEntryAddr3,
	attr: SqEntryAttr,
	optval: u64,
	x_full: [u8; 16],
}

const _: () = {
	assert!(size_of::<SqEntryFinal>() == size_of_field!(SqEntryFinal, x_full));
};

struct CompletionQueue {
	head: NonNull<AtomicU32>,
	tail: NonNull<AtomicU32>,
	flags: NonNull<AtomicU32>,
	overflow: NonNull<AtomicU32>,
	mask: u32,
	entries: u32,
	cqes: NonNull<[CqEntry]>,
}

#[repr(C)]
struct CqEntry {
	user_data: u64,
	res: i32,
	flags: u32,
}

const OFF_SQ_RING: libc::off_t = 0;
const OFF_CQ_RING: libc::off_t = 0x8000000;
const OFF_SQES: libc::off_t = 0x10000000;

pub struct IoUring {
	sq: SubmissionQueue,
	cq: CompletionQueue,

	sq_map: Mmap,
	sqes_map: Mmap,
	cq_map: Option<Mmap>,

	ring_fd: OwnedFd,
}

impl IoUring {
	pub fn new(options: IoUringOptions) -> SysResult<Self> {
		let setup_flags = IoUringSetupFlags::SINGLE_ISSUER | IoUringSetupFlags::NO_SQARRAY;

		let mut params = IoUringParams {
			flags: setup_flags,
			..IoUringParams::default()
		};

		let ring_fd = io_uring_setup(options.sq_entries, &mut params)?;

		if !params.features.contains(IoUringFeatures::NODROP) {
			return Err(SysError::from_errno(libc::ENOTSUP));
		}

		let sq_size = (params.sq_off.array as usize) + ((params.sq_entries as usize) * size_of::<libc::c_uint>());
		let cq_size = (params.cq_off.cqes as usize) + (params.cq_entries as usize * size_of::<CqEntry>());
		let (sq_size, cq_size) = if params.features.contains(IoUringFeatures::SINGLE_MMAP) {
			let single = sq_size.max(cq_size);
			(single, single)
		} else {
			(cq_size, sq_size)
		};

		let sqes_size = params.sq_entries as usize * size_of::<SqEntry>();

		let sq_map = Mmap::new_options(
			sq_size,
			MmapProtect::ReadWrite,
			MmapFlags::SHARED | MmapFlags::POPULATE,
			(ring_fd.as_raw_fd(), OFF_SQ_RING),
		)?;
		let sq_ptr = sq_map.as_non_null_ptr();

		let (cq_map, cq_ptr) = if params.features.contains(IoUringFeatures::SINGLE_MMAP) {
			(None, sq_ptr)
		} else {
			let cq_map = Mmap::new_options(
				cq_size,
				MmapProtect::ReadWrite,
				MmapFlags::SHARED | MmapFlags::POPULATE,
				(ring_fd.as_raw_fd(), OFF_CQ_RING),
			)?;
			let cq_ptr = cq_map.as_non_null_ptr();
			(Some(cq_map), cq_ptr)
		};

		let sqes_map = Mmap::new_options(
			sqes_size,
			MmapProtect::ReadWrite,
			MmapFlags::SHARED | MmapFlags::POPULATE,
			(ring_fd.as_raw_fd(), OFF_SQES),
		)?;

		let sq_head = NonNull::new(sq_ptr.as_ptr().wrapping_byte_add(params.sq_off.head as usize))
			.ok_or_else(|| SysError::from_errno(libc::ENOMEM))?
			.cast::<AtomicU32>();
		let sq_tail = NonNull::new(sq_ptr.as_ptr().wrapping_byte_add(params.sq_off.tail as usize))
			.ok_or_else(|| SysError::from_errno(libc::ENOMEM))?
			.cast::<AtomicU32>();
		let sq_ring_mask = NonNull::new(sq_ptr.as_ptr().wrapping_byte_add(params.sq_off.ring_mask as usize))
			.ok_or_else(|| SysError::from_errno(libc::ENOMEM))?
			.cast::<AtomicU32>();
		let sq_ring_mask = unsafe { sq_ring_mask.as_ref().load(Ordering::Acquire) };
		let sq_ring_entries = NonNull::new(sq_ptr.as_ptr().wrapping_byte_add(params.sq_off.ring_entries as usize))
			.ok_or_else(|| SysError::from_errno(libc::ENOMEM))?
			.cast::<AtomicU32>();
		let sq_ring_entries = unsafe { sq_ring_entries.as_ref().load(Ordering::Acquire) };
		let sq_flags = NonNull::new(sq_ptr.as_ptr().wrapping_byte_add(params.sq_off.flags as usize))
			.ok_or_else(|| SysError::from_errno(libc::ENOMEM))?
			.cast::<AtomicU32>();
		let sq_dropped = NonNull::new(sq_ptr.as_ptr().wrapping_byte_add(params.sq_off.dropped as usize))
			.ok_or_else(|| SysError::from_errno(libc::ENOMEM))?
			.cast::<AtomicU32>();
		let sq_sqes =
			NonNull::slice_from_raw_parts(sqes_map.as_non_null_ptr().cast::<SqEntry>(), params.sq_entries as usize);

		let cq_head = NonNull::new(cq_ptr.as_ptr().wrapping_byte_add(params.cq_off.head as usize))
			.ok_or_else(|| SysError::from_errno(libc::ENOMEM))?
			.cast::<AtomicU32>();
		let cq_tail = NonNull::new(cq_ptr.as_ptr().wrapping_byte_add(params.cq_off.tail as usize))
			.ok_or_else(|| SysError::from_errno(libc::ENOMEM))?
			.cast::<AtomicU32>();
		let cq_ring_mask = NonNull::new(cq_ptr.as_ptr().wrapping_byte_add(params.cq_off.ring_mask as usize))
			.ok_or_else(|| SysError::from_errno(libc::ENOMEM))?
			.cast::<AtomicU32>();
		let cq_ring_mask = unsafe { cq_ring_mask.as_ref().load(Ordering::Acquire) };
		let cq_ring_entries = NonNull::new(cq_ptr.as_ptr().wrapping_byte_add(params.cq_off.ring_entries as usize))
			.ok_or_else(|| SysError::from_errno(libc::ENOMEM))?
			.cast::<AtomicU32>();
		let cq_ring_entries = unsafe { cq_ring_entries.as_ref().load(Ordering::Acquire) };
		static CQ_FLAGS_NONE: AtomicU32 = const { AtomicU32::new(0) };
		let cq_flags = if params.cq_off.flags != 0 {
			NonNull::new(cq_ptr.as_ptr().wrapping_byte_add(params.cq_off.flags as usize))
				.ok_or_else(|| SysError::from_errno(libc::ENOMEM))?
				.cast::<AtomicU32>()
		} else {
			NonNull::from_ref(&CQ_FLAGS_NONE)
		};
		let cq_overflow = NonNull::new(cq_ptr.as_ptr().wrapping_byte_add(params.cq_off.overflow as usize))
			.ok_or_else(|| SysError::from_errno(libc::ENOMEM))?
			.cast::<AtomicU32>();
		let cq_cqes = NonNull::slice_from_raw_parts(
			NonNull::new(cq_ptr.as_ptr().wrapping_byte_add(params.cq_off.cqes as usize))
				.ok_or_else(|| SysError::from_errno(libc::ENOMEM))?
				.cast::<CqEntry>(),
			params.cq_entries as usize,
		);

		let sq = SubmissionQueue {
			head: sq_head,
			tail: sq_tail,
			flags: sq_flags,
			dropped: sq_dropped,
			mask: sq_ring_mask,
			entries: sq_ring_entries,
			sqes: sq_sqes,
			sqpoll: setup_flags.contains(IoUringSetupFlags::SQPOLL),
			// todo: acquire?
			sqe_tail: Cell::new(unsafe { sq_tail.as_ref() }.load(Ordering::Acquire)),
		};

		let cq = CompletionQueue {
			head: cq_head,
			tail: cq_tail,
			flags: cq_flags,
			overflow: cq_overflow,
			mask: cq_ring_mask,
			entries: cq_ring_entries,
			cqes: cq_cqes,
		};

		Ok(Self {
			sq_map,
			sqes_map,
			cq_map,
			ring_fd,
			sq,
			cq,
		})
	}

	pub fn prep_read(&self, fd: RawFd) -> SysResult<()> {
		let Some(sqe) = self.sq.get_sqe() else {
			return Err(SysError::from_errno(libc::EBUSY));
		};

		todo!()
	}
}
