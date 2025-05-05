use std::{
	cmp::Ordering,
	fs::OpenOptions,
	io::ErrorKind,
	net::{IpAddr, Ipv4Addr, SocketAddr},
	os::fd::{BorrowedFd, FromRawFd, RawFd},
	str::FromStr,
	sync::Arc,
	time::{Duration, SystemTime},
};

use dashmap::DashMap;
use fenrir_rs::{NetworkingBackend, SerializationFormat};
use fern::{
	colors::{Color, ColoredLevelConfig},
	Dispatch,
};
use nix::sys::{socket::SockType, stat::SFlag};
use sd_notify::NotifyState;
use stts_speech_to_text::SttStreamingState;
use tokio::{
	net::{TcpListener, TcpStream},
	time::Instant,
};
use url::Url;
use uuid::Uuid;

#[macro_use]
extern crate tracing;

fn main() {
	tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.worker_threads(4)
		.build()
		.unwrap()
		.block_on(main_inner());
}

fn init_logging() {
	// configure colors for the whole line
	let colors_line = ColoredLevelConfig::new()
		.error(Color::Red)
		.warn(Color::Yellow)
		// we actually don't need to specify the color for debug and info, they are white by default
		.info(Color::White)
		.debug(Color::White)
		// depending on the terminals color scheme, this is the same as the background color
		.trace(Color::BrightBlack);

	// configure colors for the name of the level.
	// since almost all of them are the same as the color for the whole line, we
	// just clone `colors_line` and overwrite our changes
	let colors_level = colors_line.info(Color::Green);

	let builder = fenrir_rs::Fenrir::builder()
		.endpoint(
			Url::parse(&std::env::var("LOKI_TARGET").expect("no loki URL set"))
				.expect("invalid loki url"),
		)
		.flush_threshold(1_000_000) // prevent overloading loki with too many open connections
		.network(NetworkingBackend::Reqwest)
		.format(SerializationFormat::Json)
		.include_level()
		.tag("service", "stt-service")
		.tag(
			"env",
			if cfg!(debug_assertions) {
				"dev"
			} else {
				"prod"
			},
		);
	let fenrir = builder.build();

	let mut logger = Dispatch::new()
		.chain(
			Dispatch::new()
				// configure this logger instance to make a fancy, colorful output
				.format(move |out, message, record| {
					out.finish(format_args!(
						"{color_line}[{date} {level} {target} {color_line}] {message}\x1B[0m",
						color_line = format_args!(
							"\x1B[{}m",
							colors_line.get_color(&record.level()).to_fg_str()
						),
						date = humantime::format_rfc3339(SystemTime::now()),
						target = record.target(),
						level = colors_level.color(record.level()),
						message = message,
					));
				})
				// just log messages with INFO or higher log level
				.level(tracing::log::LevelFilter::Info)
				// completely ignore ureq logs
				.level_for("ureq", tracing::log::LevelFilter::Off)
				// boost fenrir_rs logs to TRACE
				.level_for("fenrir_rs", tracing::log::LevelFilter::Trace)
				// quieten tracing spans
				.level_for("tracing::span", tracing::log::LevelFilter::Off)
				// print this setup of log messages to the console
				.chain(std::io::stdout()),
		)
		.chain(
			Dispatch::new()
				// output a raw message
				.format(|out, message, _record| out.finish(format_args!("{}", message)))
				// log everything
				.level(tracing::log::LevelFilter::Trace)
				// send this setup of log messages to Loki
				.chain(Box::new(fenrir) as Box<dyn tracing::log::Log>),
		);

	if !running_under_system_manager() {
		logger = logger.chain(
			Dispatch::new()
				// configure this logger instance to make a slightly more informative output, but without colors
				.format(move |out, message, record| {
					out.finish(format_args!(
						"[{date} {level} {target}] {message}",
						date = humantime::format_rfc3339(SystemTime::now()),
						target = record.target(),
						level = record.level(),
						message = message,
					))
				})
				// log most things
				.level(tracing::log::LevelFilter::Debug)
				// again, ignore tracing spans
				.level_for("tracing::span", tracing::log::LevelFilter::Off)
				// quieten ureq logs
				.level_for("ureq", tracing::log::LevelFilter::Warn)
				// quieten hyper
				.level_for("hyper", tracing::log::LevelFilter::Warn)
				// quieten h2
				.level_for("h2", tracing::log::LevelFilter::Warn)
				// quieten rustls
				.level_for("rustls", tracing::log::LevelFilter::Warn)
				// send this setup of log messages to a file named `output.log`
				.chain(
					OpenOptions::new()
						.write(true)
						.create(true)
						.truncate(true)
						.open("output.log")
						.expect("failed to open output.log"),
				),
		);
	}
	logger.apply().expect("failed to init logger");
}

async fn main_inner() {
	stts_speech_to_text::install_log_trampoline();
	init_logging();

	info!("loading models");
	stts_speech_to_text::load_models(
		&std::env::args()
			.nth(1)
			.expect("first argument should be path to model"),
		std::env::args()
			.nth(2)
			.expect("second argument should be how many instances of model to load")
			.parse::<usize>()
			.expect("failed to parse number of instances as usize"),
	);
	info!("loaded models");

	let socket = get_listen_socket().await;

	let (shutdown_signal_tx, _shutdown_signal_rx) = tokio::sync::broadcast::channel(1);
	let inner_shutdown_signal_tx = shutdown_signal_tx.clone();
	tokio::spawn(async move {
		tokio::signal::ctrl_c()
			.await
			.expect("failed to listen for ctrl+c");
		inner_shutdown_signal_tx
			.send(())
			.expect("failed to send shutdown signal");
	});

	let get_next_watchdog_pet_instant: Box<dyn Fn() -> Instant> =
		if let Some(watchdog_pet_interval) = watchdog_required() {
			let watchdog_pet_interval = watchdog_pet_interval / 2;
			Box::new(move || Instant::now() + watchdog_pet_interval)
		} else {
			Box::new(|| Instant::now() + Duration::from_secs(u32::MAX as u64))
		};

	let stt_streams = Arc::new(DashMap::<Uuid, SttStreamingState>::new());
	let mut next_watchdog_pet = get_next_watchdog_pet_instant();

	if running_under_system_manager() {
		sd_notify::notify(false, &[NotifyState::Ready])
			.expect("failed to notify system manager of ready");
	}

	info!("polling for connections");
	loop {
		// accept connections and spawn a task for each one
		// or if ctrl+c is received, break the loop
		let conn: tokio::io::Result<(TcpStream, SocketAddr)> = tokio::select! {
			s = socket.accept() => s,
			_ = tokio::signal::ctrl_c() => break,
			_ = tokio::time::sleep_until(next_watchdog_pet) => {
				sd_notify::notify(false, &[NotifyState::Watchdog]).expect("failed to notify watchdog");
				next_watchdog_pet = get_next_watchdog_pet_instant();
				continue;
			}
		};
		debug!("accepting connection");
		match conn {
			Ok((stream, peer_address)) => {
				info!("accepted connection from {}", peer_address);
				let shutdown_rx = shutdown_signal_tx.subscribe();
				let stt_stream_clone = Arc::clone(&stt_streams);
				tokio::spawn(async move {
					let handler = stts_connection_handler::ConnectionHandler::new(
						stream,
						peer_address,
						stt_stream_clone,
						shutdown_rx,
					);
					handler.handle().await;
				});
			}
			Err(e)
				if matches!(
					e.kind(),
					ErrorKind::NotFound
						| ErrorKind::PermissionDenied
						| ErrorKind::HostUnreachable
						| ErrorKind::NetworkUnreachable
						| ErrorKind::NotConnected
						| ErrorKind::AddrInUse
						| ErrorKind::AddrNotAvailable
						| ErrorKind::NetworkDown
						| ErrorKind::NotSeekable
						| ErrorKind::QuotaExceeded
						| ErrorKind::Unsupported
						| ErrorKind::OutOfMemory
				) =>
			{
				// fatal error, give up
				if running_under_system_manager() {
					sd_notify::notify(
						false,
						&[NotifyState::Errno(
							e.raw_os_error().expect("expected a raw error") as u32,
						)],
					)
					.expect("failed to notify system manager of fatal error");
				}
				panic!("got fatal error accepting connection: {}", e);
			}
			Err(e) => {
				error!("failed to accept connection: {}", e);
			}
		}
	}
}

/// Check whether we should initialize a watchdog
///
/// # Returns
/// If a watchdog is required, returns a [`Duration`] specifying the time after which the
/// service manager (systemd) will kill this process.
///
/// If no watchdog is required, returns [`None`].
fn watchdog_required() -> Option<Duration> {
	if !running_under_system_manager() {
		return None;
	}

	let mut watchdog_delay = 0;
	let enabled = sd_notify::watchdog_enabled(false, &mut watchdog_delay);

	enabled.then(|| Duration::from_micros(watchdog_delay))
}

fn running_under_system_manager() -> bool {
	sd_notify::booted().expect("failed to figure out whether we're on systemd")
}

/// Return the [`TcpListener`] we should listen on.
///
/// This will first try fetching the socket file descriptor and using that if it exists,
/// before falling back to environment configuration and making one ourselves.
async fn get_listen_socket() -> TcpListener {
	if running_under_system_manager() {
		// see if we have one
		let mut listeners = sd_notify::listen_fds()
			.expect("failed to fetch sockets from system manager")
			.collect::<Vec<_>>();

		match listeners.len().cmp(&1) {
			Ordering::Less => {
				// fall through to making our own unless we should error
				if std::env::var("FAIL_IF_SOCKET_NOT_FOUND").is_ok() {
					panic!(
						"couldn't find a socket from system manager and FAIL_IF_SOCKET_NOT_FOUND \
						 was set"
					);
				}
			}
			Ordering::Equal => {
				// this is our file descriptor
				let Some(fd) = listeners.pop() else {
					unreachable!(
						"there is exactly one thing in the listeners array, we should always get \
						 it"
					);
				};

				info!("got raw listener {:?}", fd);
				if !verify_fd_is_socket(fd) {
					panic!("fd {} is not a socket, giving up", fd);
				}
				// SAFETY: we have verified that fd is a valid TCP socket, and we hold control
				// over the sole instance of it
				let listener = unsafe { std::net::TcpListener::from_raw_fd(fd) };

				listener
					.set_nonblocking(true)
					.expect("couldn't set nonblocking on passed in TCP socket");

				return TcpListener::from_std(listener)
					.expect("couldn't convert std TcpListener to tokio TcpListener");
			}
			Ordering::Greater => {
				panic!(
					"got more than one file descriptor to listen on, but can only listen on one: \
					 giving up! {:?}",
					listeners
				);
			}
		}
	}

	get_socket_from_env().await
}

/// Return true if `fd` is a socket.
fn verify_fd_is_socket(fd: RawFd) -> bool {
	let file_stat = nix::sys::stat::fstat(fd).expect("failed to check type of file descriptor");
	let is_socket =
		(file_stat.st_mode & SFlag::S_IFMT.bits() as u32) == SFlag::S_IFSOCK.bits() as u32;
	if !is_socket {
		return false;
	}

	let sock_type = {
		assert_ne!(fd, -1);
		// SAFETY: the file descriptor referenced by fd is guaranteed to be open
		// for the entirety of borrowed_fd's lifetime
		// and we asserted above that fd is not -1
		let borrowed_fd = unsafe { BorrowedFd::borrow_raw(fd) };

		nix::sys::socket::getsockopt(&borrowed_fd, nix::sys::socket::sockopt::SockType)
			.expect("failed to fetch type of socket")
	};
	sock_type == SockType::Stream
}

async fn get_socket_from_env() -> TcpListener {
	if std::env::var("PORT").is_ok_and(|s| s.parse::<u16>().is_ok()) {
		panic!(
			"PORT is deprecated, use LISTEN_ADDR instead: this will panic until you set PORT to \
			 something not a number or remove it"
		)
	}

	let bind_addr = match std::env::var("BIND_ADDR") {
		Ok(bind_addr) => SocketAddr::from_str(&bind_addr).expect("BIND_ADDR malformed"),
		Err(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 7269),
	};
	info!("found bind address {}, opening listen socket", bind_addr);
	let socket = TcpListener::bind(bind_addr)
		.await
		.expect("failed to open listen socket");
	info!("opened socket");

	socket
}
