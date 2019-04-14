//! Asynchonous I/O operaions.
use std::{
	fmt,
	os::raw::c_void,
	panic::{catch_unwind, AssertUnwindSafe, UnwindSafe},
	ptr::{self, NonNull},
	sync::{Arc, Mutex},
	time::Duration,
};

use crate::{
	ctx::Context,
	error::{Error, Result, SendResult},
	message::Message,
	socket::Socket,
	util::{duration_to_nng, validate_ptr},
};
use log::error;

/// An asynchronous I/O context.
///
/// Asynchronous operations are performed without blocking calling application
/// threads. Instead the application registers a “callback” function to be
/// executed when the operation is complete (whether successfully or not). This
/// callback will be executed exactly once.
///
/// The callback must not perform any blocking operations and must complete it’s
/// execution quickly. If the callback does block, this can lead ultimately to
/// an apparent "hang" or deadlock in the application.
///
/// ## Example
///
/// A simple server that will sleep for the requested number of milliseconds
/// before responding:
///
/// ```
/// use byteorder::{ByteOrder, LittleEndian};
/// use nng::{Aio, AioResult, Context, Message, Protocol, Socket};
/// use std::time::Duration;
///
/// let address = "inproc://nng/aio.rs#callbacks";
/// let num_contexts = 10;
///
/// // Create the new socket and all of the socket contexts.
/// let socket = Socket::new(Protocol::Rep0).unwrap();
/// let workers: Vec<_> = (0..num_contexts)
///     .map(|_| {
///         let ctx = Context::new(&socket).unwrap();
///         let ctx_clone = ctx.clone();
///         let aio = Aio::new(move |aio, res| callback(aio, &ctx_clone, res)).unwrap();
///         (aio, ctx)
///     })
///     .collect();
///
/// // Start listening only after the contexts are available.
/// socket.listen(address).unwrap();
///
/// // Have the workers start responding to replies.
/// for (a, c) in &workers {
///     c.recv(a).unwrap();
/// }
///
/// fn callback(aio: &Aio, ctx: &Context, res: AioResult) {
///     match res {
///         // We successfully sent the message, wait for a new one.
///         AioResult::SendOk => ctx.recv(aio).unwrap(),
///
///         // We successfully received a message.
///         AioResult::RecvOk(m) => {
///             let ms = LittleEndian::read_u64(&m);
///             aio.sleep(Duration::from_millis(ms)).unwrap();
///         },
///
///         // We successfully slept.
///         AioResult::SleepOk => {
///             let msg = Message::new().unwrap();
///             ctx.send(aio, msg).unwrap();
///         },
///
///         // Anything else is an error that will just be ignored.
///         _ => {},
///     }
/// }
/// ```
pub struct Aio
{
	/// The inner AIO bits shared by all instances of this AIO.
	///
	/// Even though there are a select number of operations that don't require
	/// any Rust locking (e.g., `nng_aio_cancel`), the process of constructing
	/// the AIO is much easier if everything is behind a mutex.
	inner: Arc<Mutex<Inner>>,

	/// The callback function.
	///
	/// This is an `Option` because we do not want the `Aio` that is inside the
	/// callback to have any sort of ownership over the callback. If it did,
	/// then there would a circlar `Arc` reference and the AIO would never be
	/// dropped. We are never going to manually call this function, so
	/// the fact that it is an option is not an issue.
	///
	/// We can assert that is is unwind safe because we literally never call
	/// this function. I don't think we could if we wanted to, which is the
	/// entire point of the black box.
	callback: Option<AssertUnwindSafe<Arc<dyn FnOnce() + Sync + Send>>>,
}

impl Aio
{
	/// Creates a new asynchronous I/O handle.
	///
	/// The provided callback will be called on every single I/O event,
	/// successful or not. It is possible that the callback will be entered
	/// multiple times simultaneously.
	///
	/// ## Panicking
	///
	/// If the callback function panics, the program will abort. This is to
	/// match the behavior specified in Rust 1.33 where the program will abort
	/// when it panics across an `extern "C"` boundary. This library will
	/// produce the abort regardless of which version of Rustc is being used.
	///
	/// The user is responsible for either having a callback that never panics
	/// or catching and handling the panic within the callback.
	pub fn new<F>(callback: F) -> Result<Self>
	where
		F: Fn(&Aio, AioResult) + Sync + Send + UnwindSafe + 'static,
	{
		// The shared inner needs to have a fixed location before we can do anything
		// else. Make sure to mark the AIO as "Building" so we don't try to free a
		// dangling pointer.
		let inner = Arc::new(Mutex::new(Inner {
			handle: AioPtr(NonNull::dangling()),
			state:  State::Building,
		}));

		// Now, create the Aio that will be stored within the callback itself.
		let cb_aio = Aio { inner: Arc::clone(&inner), callback: None };

		// Wrap the user's callback in our own state-keeping logic
		let bounce = move || {
			let res = unsafe {
				// Don't hold the lock during the callback, hence the extra frame.
				let mut l = cb_aio.inner.lock().unwrap();
				let rv = nng_sys::nng_aio_result(l.handle.ptr()) as u32;

				let res = match (l.state, rv) {
					(State::Sending, 0) => AioResult::SendOk,
					(State::Sending, e) => {
						let msgp = nng_sys::nng_aio_get_msg(l.handle.ptr());
						let msg = Message::from_ptr(NonNull::new(msgp).unwrap());
						AioResult::SendErr(msg, Error::from_code(e))
					},

					(State::Receiving, 0) => {
						let msgp = nng_sys::nng_aio_get_msg(l.handle.ptr());
						let msg = Message::from_ptr(NonNull::new(msgp).unwrap());
						AioResult::RecvOk(msg)
					},
					(State::Receiving, e) => AioResult::RecvErr(Error::from_code(e)),

					(State::Sleeping, 0) => AioResult::SleepOk,
					(State::Sleeping, e) => AioResult::SleepErr(Error::from_code(e)),

					// The code guarantees that we can't be in the `Building` state after
					// construction and I am 99% sure we can't get any events when the AIO has no
					// running operations.
					(State::Building, _) | (State::Inactive, _) => unreachable!(),
				};

				l.state = State::Inactive;
				res
			};
			callback(&cb_aio, res)
		};

		// We can avoid double boxing by taking the address of a generic function.
		// Unfortunately, we have no way to get the type of a closure other than calling
		// a generic function, so we do have to call another function to actually
		// allocate the AIO.
		let callback = Some(AssertUnwindSafe(Aio::alloc_trampoline(&inner, bounce)?));

		// If we made it here, the type is officially done building and we can mark is
		// as such.
		inner.lock().unwrap().state = State::Inactive;
		Ok(Self { inner, callback })
	}

	/// Attempts to clone the AIO object.
	///
	/// The AIO object that is passed as an argument to the callback can never
	/// be cloned. Any other instance of the AIO object can be. All clones refer
	/// to the same underlying AIO operations.
	pub fn try_clone(&self) -> Option<Self>
	{
		// The user can never, ever clone an instance of the callback AIO object. We use
		// the uniqueness of the callback pointer to know when to safely drop items. See
		// the `Drop` implementation for more details.
		if let Some(a) = &self.callback {
			let callback = Some(AssertUnwindSafe((*a).clone()));
			Some(Self { inner: Arc::clone(&self.inner), callback })
		}
		else {
			None
		}
	}

	/// Blocks the current thread until the current asynchronous operation
	/// completes.
	///
	/// If there are no operations running then this function returns
	/// immediately. This function should **not** be called from within the
	/// completion callback.
	pub fn wait(&self)
	{
		// We can't hold the lock while we wait. Lucky for us, the pointer will
		// literally never change.
		let ptr = self.inner.lock().unwrap().handle.ptr();
		unsafe {
			nng_sys::nng_aio_wait(ptr);
		}
	}

	/// Cancel the currently running I/O operation.
	pub fn cancel(&self)
	{
		// Technically we don't need to lock but it makes the construction code cleaner.
		let inner = self.inner.lock().unwrap();
		unsafe {
			nng_sys::nng_aio_cancel(inner.handle.ptr());
		}
	}

	/// Set the timeout of asynchronous operations.
	///
	/// This causes a timer to be started when the operation is actually
	/// started. If the timer expires before the operation is completed, then it
	/// is aborted with `Error::TimedOut`.
	///
	/// As most operations involve some context switching, it is usually a good
	/// idea to allow a least a few tens of milliseconds before timing them out
	/// - a too small timeout might not allow the operation to properly begin
	/// before giving up!
	pub fn set_timeout(&self, dur: Option<Duration>)
	{
		// The `nng_aio_set_timeout` function is very much not thread-safe, so we do
		// need to lock.
		let ms = duration_to_nng(dur);

		let inner = self.inner.lock().unwrap();
		unsafe {
			nng_sys::nng_aio_set_timeout(inner.handle.ptr(), ms);
		}
	}

	/// Performs and asynchronous sleep operation.
	///
	/// If the sleep finishes completely, it will never return an error. If a
	/// timeout has been set and it is shorter than the duration of the sleep
	/// operation, the sleep operation will end early with
	/// `Error::TimedOut`.
	///
	/// This function will return immediately. If there is already an I/O
	/// operation in progress, this function will return `Error::TryAgain`.
	pub fn sleep(&self, dur: Duration) -> Result<()>
	{
		let mut inner = self.inner.lock().unwrap();

		if inner.state == State::Inactive {
			let ms = duration_to_nng(Some(dur));
			unsafe {
				nng_sys::nng_sleep_aio(ms, inner.handle.ptr());
			}
			inner.state = State::Sleeping;

			Ok(())
		}
		else {
			Err(Error::TryAgain)
		}
	}

	/// Send a message on the provided socket.
	pub(crate) fn send_socket(&self, socket: &Socket, msg: Message) -> SendResult<()>
	{
		let mut inner = self.inner.lock().unwrap();

		if inner.state == State::Inactive {
			unsafe {
				nng_sys::nng_aio_set_msg(inner.handle.ptr(), msg.into_ptr().as_ptr());
				nng_sys::nng_send_aio(socket.handle(), inner.handle.ptr());
			}

			inner.state = State::Sending;
			Ok(())
		}
		else {
			Err((msg, Error::TryAgain))
		}
	}

	/// Receive a message on the provided socket.
	pub(crate) fn recv_socket(&self, socket: &Socket) -> Result<()>
	{
		let mut inner = self.inner.lock().unwrap();

		if inner.state == State::Inactive {
			unsafe {
				nng_sys::nng_recv_aio(socket.handle(), inner.handle.ptr());
			}

			inner.state = State::Receiving;
			Ok(())
		}
		else {
			Err(Error::TryAgain)
		}
	}

	/// Send a message on the provided context.
	pub(crate) fn send_ctx(&self, ctx: &Context, msg: Message) -> SendResult<()>
	{
		let mut inner = self.inner.lock().unwrap();

		if inner.state == State::Inactive {
			unsafe {
				nng_sys::nng_aio_set_msg(inner.handle.ptr(), msg.into_ptr().as_ptr());
				nng_sys::nng_ctx_send(ctx.handle(), inner.handle.ptr());
			}

			inner.state = State::Sending;
			Ok(())
		}
		else {
			Err((msg, Error::TryAgain))
		}
	}

	/// Receive a message on the provided context.
	pub(crate) fn recv_ctx(&self, ctx: &Context) -> Result<()>
	{
		let mut inner = self.inner.lock().unwrap();

		if inner.state == State::Inactive {
			unsafe {
				nng_sys::nng_ctx_recv(ctx.handle(), inner.handle.ptr());
			}

			inner.state = State::Receiving;
			Ok(())
		}
		else {
			Err(Error::TryAgain)
		}
	}

	/// Utility function for allocating an `nng_aio`.
	///
	/// We need this because, in Rustc 1.31, there is zero way to get the type
	/// of the closure other than calling a generic function.
	fn alloc_trampoline<F>(
		inner: &Arc<Mutex<Inner>>,
		bounce: F,
	) -> Result<Arc<dyn FnOnce() + Sync + Send>>
	where
		F: Fn() + Sync + Send + UnwindSafe + 'static,
	{
		let mut boxed = Box::new(bounce);

		let mut aio: *mut nng_sys::nng_aio = ptr::null_mut();
		let aiop: *mut *mut nng_sys::nng_aio = &mut aio as _;
		let rv = unsafe {
			nng_sys::nng_aio_alloc(aiop, Some(Aio::trampoline::<F>), &mut *boxed as *mut _ as _)
		};

		// NNG should never touch the pointer and return a non-zero code at the same
		// time. That being said, I'm going to be a pessimist and double check. If we do
		// encounter that case, the safest thing to do is make the pointer null again so
		// that the dropping of the inner can detect that something went south.
		//
		// This might leak memory (I'm not sure, depends on what NNG did), but a small
		// amount of lost memory is better than a segfaulting Rust library.
		if rv != 0 && !aio.is_null() {
			error!("NNG returned a non-null pointer from a failed function");
			return Err(Error::Unknown(0));
		}
		inner.lock().unwrap().handle = AioPtr(validate_ptr(rv, aio)?);

		// Put the callback in the blackbox.
		Ok(Arc::new(move || {
			let _ = boxed;
		}))
	}

	/// Trampoline function for calling a closure from C.
	///
	/// This is really unsafe because you have to be absolutely positive in that
	/// the type of the pointer is actually `F`. Because we're going through C
	/// and a `c_void`, the type system does not enforce this for us.
	extern "C" fn trampoline<F>(arg: *mut c_void)
	where
		F: Fn() + Sync + Send + UnwindSafe + 'static,
	{
		let res = catch_unwind(|| unsafe {
			let callback_ptr = arg as *const F;
			if callback_ptr.is_null() {
				// This should never happen. It means we, Nng-rs, got something wrong in the
				// allocation code.
				panic!("Null argument given to trampoline function - please open an issue");
			}

			(*callback_ptr)()
		});

		// See #6 for "discussion" about why we abort here.
		if res.is_err() {
			// No other useful information to relay to the user.
			error!("Panic in AIO callback function.");
			std::process::abort();
		}
	}
}

#[allow(clippy::use_debug)]
impl fmt::Debug for Aio
{
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result
	{
		if let Some(ref a) = self.callback {
			write!(f, "CalbackAio {{ inner: {:?}, callback: Some({:p}) }}", self.inner, a)
		}
		else {
			write!(f, "CalbackAio {{ inner: {:?}, callback: None }}", self.inner)
		}
	}
}

impl Drop for Aio
{
	fn drop(&mut self)
	{
		// This is actually a vastly critical point in the correctness of this type. The
		// inner data won't be dropped until all of the Aio objects are dropped, meaning
		// that the callback function is in the process of being shut down and may
		// already be freed by the time we get to the drop method of the Inner. This
		// means that we can't depend on the inner object to shut down the NNG AIO
		// object and we have to do that instead.
		//
		// Therefore, if we are the unique owner of the callback closure, we need to put
		// the AIO in a state where we know the callback isn't running. I *think* the
		// `nng_aio_free` function will handle this for us but the wording of the
		// documentation is a little confusing to me. Fortunately, the documentation for
		// `nng_aio_stop` is much clearer, will definitely do what we want and will also
		// allow us to leave the actual freeing to the Inner object.
		//
		// Of course, all of this depends on the user not being able to move a closure
		// Aio out of the closure. For that, all we need to do is provide it to them as
		// a borrow and do not allow it to be cloned (by them). Fortunately, if we get
		// this wrong, I _think_ the only issues will be non-responsive AIO operations.
		if let Some(ref mut a) = self.callback {
			// We share ownership of the callback, so we might need to shut things down.
			if Arc::get_mut(a).is_some() {
				// We are the only owner so we need to shut down the AIO. One thing we need to
				// watch out for is making sure we aren't holding on to the lock when we do the
				// shutdown. The stop function will block until the callback happens and the
				// callback will block until the lock is released: deadlock.
				//
				// Now, we could try to avoid this by not putting the pointer behind a mutex,
				// but that would potentially cause issues with keeping the state synchronized
				// and we would just end up using the state's lock for the pointer anyway.
				// Instead, let's take advantage of the fact that the pointer will never change,
				// the pointed-to object is behind its own lock, and we're not touching the
				// state.
				let ptr = self.inner.lock().unwrap().handle.ptr();
				unsafe { nng_sys::nng_aio_stop(ptr) }
			}
			else {
				// Just a sanity check. We need to never take a weak reference to the callback.
				// I see no reason why we would, but I'm putting this check here just in case.
				// If this panic ever happens, it is potentially a major bug.
				assert_eq!(
					Arc::weak_count(a),
					0,
					"There is a weak reference in the AIO. This is a bug - please file an issue"
				);
			}
		}
	}
}

/// The shared inner items of a `Aio`.
#[derive(Debug)]
struct Inner
{
	/// The handle to the NNG AIO object.
	handle: AioPtr,

	/// The current state of the AIO object.
	state: State,
}

impl Drop for Inner
{
	fn drop(&mut self)
	{
		// It is possible for this to be dropping while the pointer is dangling. The
		// Inner struct is created before the pointer is allocated and it will be
		// dropped with a dangling pointer if the NNG allocation fails.
		if self.state != State::Building {
			// If we are being dropped, then the callback is being dropped. If the callback
			// is being dropped, then an instance of `Aio` shut down the AIO. This will
			// either run the callback and clean up the Message memory or the AIO didn't
			// have an operation running and there is nothing to clean up. As such, we don't
			// need to do anything except free the AIO.
			unsafe {
				nng_sys::nng_aio_free(self.handle.ptr());
			}
		}
	}
}

/// The result of an AIO operation.
// There are no "Inactive" results as I don't think there is a valid way to get any type of callback
// trigger when there are no operations running. All of the "user forced" errors, such as
// cancellation or timeouts, don't happen if there are no running operations. If there are no
// running operations, then no non-"user forced" errors can happen.
#[derive(Clone, Debug)]
#[must_use]
pub enum AioResult
{
	/// The send operation was successful.
	SendOk,

	/// The send operation failed.
	///
	/// This contains the message that was being sent.
	SendErr(Message, Error),

	/// The receive operation was successful.
	RecvOk(Message),

	/// The receive operation failed.
	RecvErr(Error),

	/// The sleep operation was successful.
	SleepOk,

	/// The sleep operation failed.
	///
	/// This is almost always because the sleep was canceled and the error will
	/// usually be `Error::Canceled`.
	SleepErr(Error),
}

impl From<AioResult> for Result<Option<Message>>
{
	fn from(aio_res: AioResult) -> Result<Option<Message>>
	{
		use self::AioResult::*;

		match aio_res {
			SendOk | SleepOk => Ok(None),
			SendErr(_, e) | RecvErr(e) | SleepErr(e) => Err(e),
			RecvOk(m) => Ok(Some(m)),
		}
	}
}

/// Represents the state of the AIO object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State
{
	/// The AIO is in the process of being constructed.
	///
	/// This exists purely so that the callback forms of the AIO (which need a
	/// non-moving pointer) can signal that they failed to fully construct. This
	/// should never be seen outside of that context.
	Building,

	/// There is currently nothing happening on the AIO.
	Inactive,

	/// A send operation is currently in progress.
	Sending,

	/// A receive operation is currently in progress.
	Receiving,

	/// The AIO object is currently sleeping.
	Sleeping,
}

/// Newtype to make the `*mut nng_aio` implement `Send`.
#[repr(transparent)]
#[derive(Debug)]
struct AioPtr(NonNull<nng_sys::nng_aio>);
impl AioPtr
{
	/// Returns the wrapped pointer.
	fn ptr(&self) -> *mut nng_sys::nng_aio { self.0.as_ptr() }
}
unsafe impl Send for AioPtr {}
