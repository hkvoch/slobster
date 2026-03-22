mod mman;

use core::fmt;
use std::any::Any;
use std::cell::{Cell, RefCell, UnsafeCell};
use std::marker::PhantomPinned;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::pin::Pin;
use std::ptr::{NonNull, null_mut};
use std::rc::Rc;
use std::{assert_matches, debug_assert_matches};

use pin_project_lite::pin_project;

use crate::utils::tl_swap;
pub(crate) use mman::*;

pub(crate) struct Coroutine {
	inner: Rc<CoroutineInner>,
}

enum CoroutineState {
	NotStarted(Box<dyn FnOnce() + 'static>),
	Running,
	Returned,
	Panicked(Box<dyn Any + Send>),
}

impl fmt::Debug for CoroutineState {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			| Self::NotStarted(arg0) => f.debug_tuple("NotStarted").field(&((&**arg0) as *const _)).finish(),
			| Self::Running => write!(f, "Running"),
			| Self::Returned => write!(f, "Returned"),
			| Self::Panicked(arg0) => f.debug_tuple("Panicked").field(arg0).finish(),
		}
	}
}

struct CoroutineInner {
	stack: Mmap,
	context: UnsafeCell<libc::ucontext_t>,
	state: RefCell<CoroutineState>,
}

impl Coroutine {
	fn clone_internal(&self) -> Self {
		Self {
			inner: Rc::clone(&self.inner),
		}
	}

	pub(crate) fn coro_yield(&self) {
		let rt = RUNTIME_CTX.get();
		let ret = unsafe { libc::swapcontext(self.inner.context.get(), rt) };
		if ret != 0 {
			let err = std::io::Error::last_os_error();
			panic!("failed to yield: {err}");
		}
	}

	fn run(&self) {
		let state = self.inner.state.replace(CoroutineState::Running);
		match state {
			| CoroutineState::NotStarted(exec) => {
				exec();
			}
			| state => {
				self.inner.state.replace(state);
				panic!("coroutine started in invalid state")
			}
		}
	}

	pub(crate) fn done(&self) -> bool {
		match &*self.inner.state.borrow() {
			| CoroutineState::NotStarted(_) => false,
			| CoroutineState::Running => false,
			| CoroutineState::Returned => true,
			| CoroutineState::Panicked(_) => true,
		}
	}

	pub(crate) fn this() -> Self { THIS_COROUTINE.with_borrow(move |coro| coro.as_ref().unwrap().clone_internal()) }
}

impl fmt::Debug for Coroutine {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Coroutine")
			.field("stack", &self.inner.stack.as_non_null())
			.field("state", &self.inner.state)
			.finish_non_exhaustive()
	}
}

thread_local! {
	static RUNTIME_CTX: Cell<*const libc::ucontext_t> = const{Cell::new(null_mut())};
	static THIS_COROUTINE: RefCell<Option<Coroutine>> = const{RefCell::new(None)};
	static CORO_RETURN: RefCell<Option<Result<(), Box<dyn Any + Send>>>> = const{RefCell::new(None)};
}

pin_project! {
	pub(crate) struct Executor {
		context: libc::ucontext_t,
		#[pin]
		_unpin: PhantomPinned,
	}
}

impl Executor {
	pub(crate) fn new() -> Result<Self, ()> {
		let mut context = MaybeUninit::uninit();
		let ret = unsafe { libc::getcontext(context.as_mut_ptr()) };
		if ret != 0 {
			return Err(());
		}

		Ok(Self {
			context: unsafe { context.assume_init() },
			_unpin: PhantomPinned,
		})
	}

	pub(crate) fn new_coro<F>(self: Pin<&mut Self>, exec: F) -> Coroutine
	where F: FnOnce() + 'static {
		let mut context = MaybeUninit::uninit();
		let ret = unsafe { libc::getcontext(context.as_mut_ptr()) };
		if ret != 0 {
			panic!(
				"failed to create coroutine context: {}",
				std::io::Error::last_os_error()
			);
		}

		let stack_alloc = (1usize << 20) + get_page_size().get();
		let stack = Mmap::new(stack_alloc).expect("failed to allocate coroutine stack");

		{
			let context = unsafe { context.assume_init_mut() };
			context.uc_stack.ss_sp = stack.as_non_null().cast().as_ptr();
			context.uc_stack.ss_size = stack.len();
			context.uc_link = &raw mut *self.project().context;
		}

		unsafe {
			libc::makecontext(context.as_mut_ptr(), Self::run_this, 0);
		};

		let coro = CoroutineInner {
			state: RefCell::new(CoroutineState::NotStarted(Box::new(exec))),
			context: UnsafeCell::new(unsafe { context.assume_init() }),
			stack,
		};
		let inner = Rc::new(coro);

		Coroutine { inner }
	}

	pub(crate) fn enter(mut self: Pin<&mut Self>, coro: &Coroutine) {
		let this = self.project();

		assert_matches!(
			&*coro.inner.state.borrow(),
			CoroutineState::Running | CoroutineState::NotStarted(_)
		);

		tl_swap!(Some(coro.clone_internal()) => THIS_COROUTINE; {
			tl_swap!(@cell &raw mut *this.context => RUNTIME_CTX; {
				let ret = unsafe { libc::swapcontext(&raw mut *this.context, coro.inner.context.get()) };
				if ret != 0 {
					let err = std::io::Error::last_os_error();
					panic!("failed to enter coroutine: {err}")
				}
			});
		});

		debug_assert_matches!(&*coro.inner.state.borrow(), CoroutineState::Running);

		match CORO_RETURN.take() {
			| Some(Ok(())) => {
				coro.inner.state.replace(CoroutineState::Returned);
			}
			| Some(Err(panic)) => {
				coro.inner.state.replace(CoroutineState::Panicked(panic));
			}
			| None => {}
		}
	}

	extern "C" fn run_this() {
		let ret = std::panic::catch_unwind(|| {
			Coroutine::this().run();
		});
		CORO_RETURN.set(Some(ret));
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn simple_coroutines() {
		let mut exec = std::pin::pin!(Executor::new().unwrap());

		let coro1 = exec.as_mut().new_coro(|| {
			let me = Coroutine::this();
			eprintln!("coro 1 start");
			me.coro_yield();
			eprintln!("coro 1 resume");
		});

		let coro2 = exec.as_mut().new_coro(|| {
			let me = Coroutine::this();
			eprintln!("coro 2 start");
			me.coro_yield();
			eprintln!("coro 2 resume");
		});

		let mut backlog = vec![coro1.clone_internal(), coro2.clone_internal()];

		while !backlog.is_empty() {
			let mut i = 0usize;
			while let Some(coro) = backlog.get(i) {
				exec.as_mut().enter(coro);
				if coro.done() {
					backlog.swap_remove(i);
				} else {
					i += 1;
				}
			}
		}

		todo!();
	}
}
