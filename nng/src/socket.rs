use std::time::Duration;
use std::ffi::{CString, CStr};
use std::os::raw::c_char;
use std::ptr;

use nng_sys;
use nng_sys::protocol::*;

use error::{ErrorKind, Result, SendResult};
use message::Message;

/// A nanomsg-next-generation socket.
///
/// All communication between application and remote Scalability Protocol peers
/// is done through sockets. A given socket can have multiple dialers,
/// listeners, and pipes, and may be connected to multiple transports at the
/// same time. However, a given socket will have exactly one protocol
/// associated with it and is responsible for any state machines or other
/// application-specific logic.
///
/// See the [nng documenatation][1] for more information.
///
/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_socket.5.html
#[derive(Debug)]
pub struct Socket
{
	/// Handle to the underlying nng socket.
	handle: nng_sys::nng_socket,

	/// Whether or not this socket should block on sending and receiving
	nonblocking: bool,
}
impl Socket
{
	/// Creates a new socket which uses the specified protocol.
	pub fn new(t: Protocol) -> Result<Socket>
	{
		// Create the uninitialized nng_socket
		let mut socket = nng_sys::NNG_SOCKET_INITIALIZER;

		// Try to open a socket of the specified type
		let rv = unsafe {
			match t {
				Protocol::Bus0 => bus0::nng_bus0_open(&mut socket as *mut _),
				Protocol::Pair0 => pair0::nng_pair0_open(&mut socket as *mut _),
				Protocol::Pair1 => pair1::nng_pair1_open(&mut socket as *mut _),
				Protocol::Pub0 => pubsub0::nng_pub0_open(&mut socket as *mut _),
				Protocol::Pull0 => pipeline0::nng_pull0_open(&mut socket as *mut _),
				Protocol::Push0 => pipeline0::nng_pull0_open(&mut socket as *mut _),
				Protocol::Rep0 => reqrep0::nng_rep0_open(&mut socket as *mut _),
				Protocol::Req0 => reqrep0::nng_req0_open(&mut socket as *mut _),
				Protocol::Respondent0 => survey0::nng_respondent0_open(&mut socket as *mut _),
				Protocol::Sub0 => pubsub0::nng_sub0_open(&mut socket as *mut _),
				Protocol::Surveyor0 => survey0::nng_surveyor0_open(&mut socket as *mut _),
			}
		};

		rv2res!(rv, Socket { handle: socket, nonblocking: false })
	}

	/// Initiates a remote connection to a listener.
	///
	/// When the connection is closed, the underlying `Dialer` will attempt to
	/// re-establish the connection. It will also periodically retry a
	/// connection automatically if an attempt to connect asynchronously fails.
	///
	/// Normally, the first attempt to connect to the address indicated by the
	/// provided _url_ is done synchronously, including any necessary name
	/// resolution. As a result, a failure, such as if the connection is
	/// refused, will be returned immediately and no further action will be
	/// taken.
	///
	/// However, if the socket is set to `nonblocking`, then the connection
	/// attempt is made asynchronously.
	///
	/// Furthermore, if the connection was closed for a synchronously dialed
	/// connection, the dialer will still attempt to redial asynchronously.
	///
	/// Because the dialer is started immediately, it is generally not possible
	/// to apply extra configuration. If that is needed, or if one wishes to
	/// close the dialer before the socket, applications should consider using
	/// the `Dialer` type directly.
	///
	/// See the [nng documentation][1] for more information.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_dial.3.html
	pub fn dial(&mut self, url: &str) -> Result<()>
	{
		let addr = CString::new(url).map_err(|_| ErrorKind::AddressInvalid)?;
		let flags = if self.nonblocking { nng_sys::NNG_FLAG_NONBLOCK } else { 0 };

		let rv = unsafe {
			nng_sys::nng_dial(self.handle, addr.as_ptr(), ptr::null_mut(), flags)
		};

		rv2res!(rv)
	}

	/// Initiates and starts a listener on the specified address.
	///
	/// Listeners are used to accept connections initiated by remote dialers.
	/// Unlike a dialer, listeners generally can have many connections open
	/// concurrently.
	///
	/// Normally, the act of "binding" to the address indicated by _url_ is
	/// done synchronously, including any necessary name resolution. As a
	/// result, a failure, such as if the address is already in use, will be
	/// returned immediately. However, if the socket is set to `nonblocking`
	/// then this is done asynchronously; furthermore any failure to bind will
	/// be periodically reattempted in the background.
	///
	/// Because the listener is started immediately, it is generally not
	/// possible to apply extra configuration. If that is needed, or if one
	/// wishes to close the dialer before the socket, applications should
	/// consider using the `Listener` type directly.
	///
	/// See the [nng documentation][1] for more information.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_listen.3.html
	pub fn listen(&mut self, url: &str) -> Result<()>
	{
		let addr = CString::new(url).map_err(|_| ErrorKind::AddressInvalid)?;
		let flags = if self.nonblocking { nng_sys::NNG_FLAG_NONBLOCK } else { 0 };

		let rv = unsafe {
			nng_sys::nng_listen(self.handle, addr.as_ptr(), ptr::null_mut(), flags)
		};

		rv2res!(rv)
	}

	/// Sets whether or not this socket should use nonblocking operations.
	///
	/// If the socket is set to nonblocking mode, then the send and receive
	/// functions return immediately even if there are no messages available or
	/// the message cannot be sent. Otherwise, the functions will wailt until
	/// the operation can complete or any configured timer expires.
	///
	/// The default is blocking operations.
	pub fn set_nonblocking(&mut self, nonblocking: bool)
	{
		self.nonblocking = nonblocking;
	}

	/// Receives a message from the socket.
	///
	/// The semantics of what receiving a message means vary from protocol to
	/// protocol, so examination of the protocol documentation is encouraged.
	/// For example, with a _req_ socket a message may only be received after a
	/// request has been sent. Furthermore, some protocols may not support
	/// receiving data at all, such as _pub_.
	pub fn recv(&mut self) -> Result<Message>
	{
		let mut msgp: *mut nng_sys::nng_msg = ptr::null_mut();
		let flags = if self.nonblocking { nng_sys::NNG_FLAG_NONBLOCK } else { 0 };

		let rv = unsafe {
			nng_sys::nng_recvmsg(self.handle, &mut msgp as _, flags)
		};

		validate_ptr!(rv, msgp);
		Ok(unsafe { Message::from_ptr(msgp) })
	}

	/// Sends a message on the socket.
	///
	/// The semantics of what sending a message means vary from protocol to
	/// protocol, so examination of the protocol documentation is encouraged.
	/// For example, with a _pub_ socket the data is broadcast so that any
	/// peers who have a suitable subscription will be able to receive it.
	/// Furthermore, some protocols may not support sending data (such as
	/// _sub_) or may require other conditions. For example, _rep_sockets
	/// cannot normally send data, which are responses to requests, until they
	/// have first received a request.
	///
	/// If the message cannot be sent, then it is returned to the caller as a
	/// part of the `Error`.
	pub fn send(&mut self, data: Message) -> SendResult<()>
	{
		let flags = if self.nonblocking { nng_sys::NNG_FLAG_NONBLOCK } else { 0 };

		let rv = unsafe {
			nng_sys::nng_sendmsg(self.handle, data.msgp(), flags)
		};

		if rv != 0 {
			Err((data, ErrorKind::from_code(rv).into()))
		} else {
			Ok(())
		}
	}

	/// Get the positive identifier for the socket.
	pub fn id(&self) -> i32
	{
		let id = unsafe { nng_sys::nng_socket_id(self.handle) };
		assert!(id > 0, "Invalid socket ID returned from valid socket");

		id
	}

	/// Returns the underlying `nng_socket`.
	pub(crate) fn handle(&self) -> nng_sys::nng_socket
	{
		self.handle
	}
}

impl Socket
{
	/// Whether or not this socket is in "raw" mode.
	///
	/// If `true`, the socket is in "raw" mode and if `false` the socket is in
	/// "cooked" mode. Raw mode sockets generally do not have any
	/// protocol-specific semantics applied to them; instead the application is
	/// expected to perform such semantics itself.
	///
	/// For example, in "cooked" mode a _rep_ socket would automatically copy
	/// message headers from a received message to the corresponding reply,
	/// whereas in "raw" mode this is not done.
	///
	/// See [raw mode][1] for more detail.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng.7.html#raw_mode
	pub fn raw(&self) -> Result<bool>
	{
		let mut raw = false;
		let rv = unsafe {
			nng_sys::nng_getopt_bool(self.handle, nng_sys::NNG_OPT_RAW, &mut raw as _)
		};

		rv2res!(rv, raw)
	}

	/// The minimum amount of time to wait before trying to establish a
	/// connection after a previous attempt has failed.
	///
	/// This is the default time. Individual dialers can override this setting.
	/// This option is irrelevant for listeners. A value of `None` indicates an
	/// infinite timeout.
	pub fn reconnect_min_time(&self) -> Result<Option<Duration>>
	{
		let mut dur: nng_sys::nng_duration = 0;
		let rv = unsafe {
			nng_sys::nng_getopt_ms(self.handle, nng_sys::NNG_OPT_RECONNMINT, &mut dur as _)
		};

		rv2res!(rv, super::nng_to_duration(dur))
	}

	/// Sets the minimum amount of time to wait before trying to establish a
	/// connection after a previous attempt has failed.
	///
	/// Individual dialers can override this setting. This option is irrelevant
	/// for listeners. A value of `None` indicates an infinite timeout.
	pub fn set_reconnect_min_time<D>(&mut self, dur: D) -> Result<&mut Self>
		where D: Into<Option<Duration>>
	{
		let t = super::duration_to_nng(dur.into());
		let rv = unsafe {
			nng_sys::nng_setopt_ms(self.handle, nng_sys::NNG_OPT_RECONNMINT, t)
		};

		rv2res!(rv, self)
	}

	/// The maximum amount of time to wait before trying to establish a
	/// connection after a previous attempt has failed.
	///
	/// If this is non-zero, then the time between successive connection
	/// attempts will start at the minimum reconnect time and grow
	/// exponentially until it reaches the provided value. If this is not set
	/// or set to zero, then no exponential back-off is done and each attempt
	/// will wait the time specified by the minimum reconnect time.
	///
	/// This is the default value and can be overridden by individual dialers.
	/// This option is irrelevant for listeners. A value of `None` indicates an
	/// infinite timeout.
	pub fn reconnect_max_time(&self) -> Result<Option<Duration>>
	{
		let mut dur: nng_sys::nng_duration = 0;
		let rv = unsafe {
			nng_sys::nng_getopt_ms(self.handle, nng_sys::NNG_OPT_RECONNMAXT, &mut dur as _)
		};

		rv2res!(rv, super::nng_to_duration(dur))
	}

	/// Sets the maximum amount of time to wait before trying to establish a
	/// connection after a previous attempt has failed.
	///
	/// If this is non-zero, then the time between successive connection
	/// attempts will start at the minimum reconnect time and grow
	/// exponentially until it reaches the provided value. If this is not set
	/// or set to zero, then no exponential back-off is done and each attempt
	/// will wait the time specified by the minimum reconnect time.
	///
	/// Individual dialers can override this setting. This option is irrelevant
	/// for listeners. A value of `None` indicates an infinite timeout.
	pub fn set_reconnect_max_time<D>(&mut self, dur: D) -> Result<&mut Self>
		where D: Into<Option<Duration>>
	{
		let t = super::duration_to_nng(dur.into());
		let rv = unsafe {
			nng_sys::nng_setopt_ms(self.handle, nng_sys::NNG_OPT_RECONNMAXT, t)
		};

		rv2res!(rv, self)
	}

	/// The depth of the socket's receive buffer as a number of messages.
	///
	/// Messages received by a transport my be buffered until the application
	/// has accepted them for delivery.
	pub fn recv_buf_depth(&self) -> Result<i32>
	{
		let mut sz: i32 = 0;
		let rv = unsafe {
			nng_sys::nng_getopt_int(self.handle, nng_sys::NNG_OPT_RECVBUF, &mut sz as _)
		};

		rv2res!(rv, sz)
	}

	/// Sets the depth of the socket's receive buffer as a number of messages.
	///
	/// Messages received by a transport my be buffered until the application
	/// has accepted them for delivery.
	pub fn set_recv_buf_depth(&mut self, size: i32) -> Result<&mut Self>
	{
		let rv = unsafe {
			nng_sys::nng_setopt_int(self.handle, nng_sys::NNG_OPT_RECVBUF, size)
		};

		rv2res!(rv, self)
	}

	/// The maximum message size that will be accepted from a remote peer.
	///
	/// If a peer attempts to send a message larger than this, then the message
	/// will be discarded. If the value of this is zero, then no limit on
	/// message sizes is enforced. This option exists to prevent certain kinds
	/// of denial-of-service attacks, where a malicious agent can claim to want
	/// to send an extraordinarily large message without sending any data.
	///
	/// This is the default value which can be overridden on a per-dialer or
	/// per-listener basis.
	pub fn recv_max_size(&self) -> Result<usize>
	{
		let mut sz: usize = 0;
		let rv = unsafe {
			nng_sys::nng_getopt_size(self.handle, nng_sys::NNG_OPT_RECVMAXSZ, &mut sz as _)
		};

		rv2res!(rv, sz)
	}

	/// Set the maximum message size that will be accepted from a remote peer.
	///
	/// If a peer attempts to send a message larger than this, then the message
	/// will be discarded. If the value of this is zero, then no limit on
	/// message sizes is enforced. This option exists to prevent certain kinds
	/// of denial-of-service attacks, where a malicious agent can claim to want
	/// to send an extraordinarily large message without sending any data.
	///
	/// This is the default value which can be overridden on a per-dialer or
	/// per-listener basis.
	pub fn set_recv_max_size(&mut self, sz: usize) -> Result<&mut Self>
	{
		let rv = unsafe {
			nng_sys::nng_setopt_size(self.handle, nng_sys::NNG_OPT_RECVMAXSZ, sz)
		};

		rv2res!(rv, self)
	}

	/// The socket receive timeout.
	///
	/// When no messages are available for receiving at the socket for this
	/// period of time, receive operations will fail. A value of `None`
	/// indicates an infinite timeout.
	pub fn recv_timeout(&self) -> Result<Option<Duration>>
	{
		let mut dur: nng_sys::nng_duration = 0;
		let rv = unsafe {
			nng_sys::nng_getopt_ms(self.handle, nng_sys::NNG_OPT_RECVTIMEO, &mut dur as _)
		};

		rv2res!(rv, super::nng_to_duration(dur))
	}

	/// Set the socket receive timeout.
	///
	/// When no messages are available for receiving at the socket for this
	/// period of time, receive operations will fail. A value of `None`
	/// indicates an infinite timeout.
	pub fn set_recv_timeout<D>(&mut self, dur: D) -> Result<&mut Self>
		where D: Into<Option<Duration>>
	{
		let t = super::duration_to_nng(dur.into());
		let rv = unsafe {
			nng_sys::nng_setopt_ms(self.handle, nng_sys::NNG_OPT_RECVTIMEO, t)
		};

		rv2res!(rv, self)
	}

	/// The depth of the socket send buffer as a number of messages.
	///
	/// Messages sent by an application may be buffered by the socket until a
	/// transport is ready to accept them for delivery. This value must be
	/// between 0 and 8192, inclusive.
	pub fn send_buf_depth(&self) -> Result<i32>
	{
		let mut sz: i32 = 0;
		let rv = unsafe {
			nng_sys::nng_getopt_int(self.handle, nng_sys::NNG_OPT_SENDBUF, &mut sz as _)
		};

		rv2res!(rv, sz)
	}

	/// Set the depth of the socket send buffer as a number of messages.
	///
	/// Messages sent by an application may be buffered by the socket until a
	/// transport is ready to accept them for delivery. This value must be
	/// between 0 and 8192, inclusive.
	pub fn set_send_buf_depth(&mut self, size: i32) -> Result<&mut Self>
	{
		let rv = unsafe {
			nng_sys::nng_setopt_int(self.handle, nng_sys::NNG_OPT_SENDBUF, size)
		};

		rv2res!(rv, self)
	}

	/// The socket send timeout.
	///
	/// When a message cannot be queued for delivery by the socket for this
	/// period of time (such as if send buffers are full), the operation will
	/// fail. A value of `None` indicates an infinite timeout.
	pub fn send_timeout(&self) -> Result<Option<Duration>>
	{
		let mut dur: nng_sys::nng_duration = 0;
		let rv = unsafe {
			nng_sys::nng_getopt_ms(self.handle, nng_sys::NNG_OPT_SENDTIMEO, &mut dur as _)
		};

		rv2res!(rv, super::nng_to_duration(dur))
	}

	/// Set the socket send timeout.
	///
	/// When a message cannot be queued for delivery by the socket for this
	/// period of time (such as if send buffers are full), the operation will
	/// fail. A value of `None` indicates an infinite timeout.
	pub fn set_send_timeout<D>(&mut self, dur: D) -> Result<&mut Self>
		where D: Into<Option<Duration>>
	{
		let t = super::duration_to_nng(dur.into());
		let rv = unsafe {
			nng_sys::nng_setopt_ms(self.handle, nng_sys::NNG_OPT_SENDTIMEO, t)
		};

		rv2res!(rv, self)
	}

	/// The socket name.
	///
	/// By default, this is a string corresponding to the value of the socket.
	/// The string must fit within 63-bytes but it can be changed for other
	/// application use.
	pub fn name(&self) -> Result<String>
	{
		unsafe {
			let mut ptr: *mut c_char = ptr::null_mut();
			let rv = nng_sys::nng_getopt_string(self.handle, nng_sys::NNG_OPT_SOCKNAME, &mut ptr as *mut _);

			if rv != 0 {
				return Err(ErrorKind::from_code(rv).into());
			}

			assert!(ptr != ptr::null_mut(), "Nng returned a null pointer from a successful function");
			let name = CStr::from_ptr(ptr).to_string_lossy().into_owned();
			nng_sys::nng_strfree(ptr);

			Ok(name)
		}
	}

	/// Set the socket name.
	///
	/// By default, this is a string corresponding to the value of the socket.
	/// The string must fit within 63-bytes but it can be changed for other
	/// application use.
	pub fn set_name(&mut self, name: &str) -> Result<&mut Self>
	{
		let cname = CString::new(name).map_err(|_| ErrorKind::InvalidInput)?;
		let rv = unsafe {
			nng_sys::nng_setopt_string(self.handle, nng_sys::NNG_OPT_SOCKNAME, cname.as_ptr())
		};

		rv2res!(rv, self)
	}

	/// The maximum number of "hops" a message may traverse.
	///
	/// The intention here is to prevent forwarding loops in device chains. Not
	/// all protocols support this option and those that do generally have a
	/// default value of 8.
	///
	/// Each node along a forwarding path may have its own value for the
	/// maximum time-to-live and performs its own checks before forwarding the
	/// message. Therefore, it is helpful if all nodes in the topology use the
	/// same value for this option.
	pub fn max_ttl(&self) -> Result<i32>
	{
		let mut ttl: i32 = 0;
		let rv = unsafe {
			nng_sys::nng_getopt_int(self.handle, nng_sys::NNG_OPT_MAXTTL, &mut ttl as _)
		};

		rv2res!(rv, ttl)
	}

	/// Set the maximum number of "hops" a message may traverse.
	///
	/// The intention here is to prevent forwarding loops in device chains. Not
	/// all protocols support this option and those that do generally have a
	/// default value of 8.
	///
	/// Each node along a forwarding path may have its own value for the
	/// maximum time-to-live and performs its own checks before forwarding the
	/// message. Therefore, it is helpful if all nodes in the topology use the
	/// same value for this option.
	pub fn set_max_ttl(&mut self, ttl: u8) -> Result<&mut Self>
	{
		let rv = unsafe {
			nng_sys::nng_setopt_int(self.handle, nng_sys::NNG_OPT_MAXTTL, ttl as i32)
		};

		rv2res!(rv, self)
	}
}

impl Drop for Socket
{
	fn drop(&mut self)
	{
		// Closing a socket should only ever return success or ECLOSED and both
		// of those mean we have nothing to drop. However, just to be sane
		// about it all, we'll warn the user if we see something odd. If that
		// ever happens, hopefully it will make its way to a bug report.
		let rv = unsafe { nng_sys::nng_close(self.handle) };
		assert!(
			rv == 0 || rv == nng_sys::NNG_ECLOSED,
			"Unexpected error code while closing socket ({})", rv
		);
	}
}

/// Protocols available for use by sockets.
#[derive(Debug)]
pub enum Protocol
{
	/// Version 0 of the bus protocol.
	///
	/// The _bus_ protocol provides for building mesh networks where every peer
	/// is connected to every other peer. In this protocol, each message sent
	/// by a node is sent to every one of its directly connected peers. See
	/// the [bus documentation][1] for more information.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_bus.7.html
	Bus0,

	/// Version 0 of the pair protocol.
	///
	/// The _pair_ protocol implements a peer-to-peer pattern, where
	/// relationships between peers are one-to-one. See the
	/// [pair documentation][1] for more information.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_pair.7.html
	Pair0,

	/// Version 1 of the pair protocol.
	///
	/// The _pair_ protocol implements a peer-to-peer pattern, where
	/// relationships between peers are one-to-one. Version 1 of this protocol
	/// supports and optional _polyamorous_ mode. See the [pair documentation][1]
	/// for more information.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_pair.7.html
	Pair1,

	/// Version 0 of the publisher protocol.
	///
	/// The _pub_ protocol is one half of a publisher/subscriber pattern. In
	/// this pattern, a publisher sends data, which is broadcast to all
	/// subscribers. The subscribing applications only see the data to which
	/// they have subscribed. See the [publisher/subscriber documentation][1]
	/// for more information.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_pub.7.html
	Pub0,

	/// Version 0 of the pull protocol.
	///
	/// The _pull_ protocol is one half of a pipeline pattern. The other half
	/// is the _push_ protocol. In the pipeline pattern, pushers distribute
	/// messages to pullers. Each message sent by a pusher will be sent to one
	/// of its peer pullers, chosen in a round-robin fashion from the set of
	/// connected peers available for receiving. This property makes this
	/// pattern useful in load-balancing scenarios.
	///
	/// See the [pipeline documentation][1] for more information.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_pull.7.html
	Pull0,

	/// Version 0 of the push protocol.
	///
	/// The _push_ protocol is one half of a pipeline pattern. The other side
	/// is the _pull_ protocol. In the pipeline pattern, pushers distribute
	/// messages to pullers. Each message sent by a pusher will be sent to one
	/// of its peer pullers, chosen in a round-robin fashion from the set of
	/// connected peers available for receiving. This property makes this
	/// pattern useful in load-balancing scenarios.
	///
	/// See the [pipeline documentation][1] for more information.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_push.7.html
	Push0,

	/// Version 0 of the reply protocol.
	///
	/// The _rep_ protocol is one half of a request/reply pattern. In this
	/// pattern, a requester sends a message to one replier, who is expected to
	/// reply. The request is resent if no reply arrives, until a reply is
	/// received or the request times out.
	///
	/// See the [request/reply documentation][1] for more information.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_rep.7.html
	Rep0,

	/// Version 0 of the request protocol.
	///
	/// The _req_ protocol is one half of a request/reply pattern. In this
	/// pattern, a requester sends a message to one replier, who is expected to
	/// reply. The request is resent if no reply arrives, until a reply is
	/// received or the request times out.
	///
	/// See the [request/reply documentation][1] for more information.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_req.7.html
	Req0,

	/// Version 0 of the respondent protocol.
	///
	/// The _respondent_ protocol is one half of a survey pattern. In this
	/// pattern, a surveyor sends a survey, which is broadcast to all peer
	/// respondents. The respondents then have a chance to reply (but are not
	/// obliged to reply). The survey itself is a timed event, so that
	/// responses received after the survey has finished are discarded.
	///
	/// See the [survery documentation][1] for more information.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_respondent.7.html
	Respondent0,

	/// Version 0 of the subscriber protocol.
	///
	/// The _sub_ protocol is one half of a publisher/subscriber pattern. In
	/// this pattern, a publisher sends data, which is broadcast to all
	/// subscribers. The subscribing applications only see the data to which
	/// they have subscribed.
	///
	/// See the [publisher/subscriber documentation][1] for more information.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_sub.7.html
	Sub0,

	/// Version 0 of the surveyor protocol.
	///
	/// The _surveyor_ protocol is one half of a survey pattern. In this
	/// pattern, a surveyor sends a survey, which is broadcast to all peer
	/// respondents. The respondents then have a chance to reply (but are not
	/// obliged to reply). The survey itself is a timed event, so that
	/// responses received after the survey has finished are discarded.
	///
	/// See the [survey documentation][1] for more information.
	///
	/// [1]: https://nanomsg.github.io/nng/man/v1.0.0/nng_surveyor.7.html
	Surveyor0,
}