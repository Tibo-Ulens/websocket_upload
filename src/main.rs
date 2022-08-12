use std::sync::Arc;
use std::{process, thread};

use anyhow::Error;
use clap::{Arg, ArgAction, Command};
use futures_util::future::join_all;
use futures_util::lock::Mutex;
use futures_util::stream::FuturesUnordered;
use futures_util::{SinkExt, StreamExt};
use image::io::Reader;
use image::GenericImageView;
use rand::seq::SliceRandom;
use rand::thread_rng;
use tokio::runtime::Runtime;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::protocol::Message;
use url::Url;

/// Open a websocket and send a chunk of messages
#[inline(always)]
fn send_chunk_ws(id: usize, msg_chunk: Vec<Message>, url: &Url, repeat: bool) {
	let runtime = Runtime::new().unwrap();

	let tx_raw = runtime.block_on(async move {
		let (stream, _) = connect_async(url).await.unwrap();
		stream.split().0
	});

	let tx = Arc::new(Mutex::new(tx_raw));

	loop {
		// TODO: try to not reallocate this every loop
		let futures = FuturesUnordered::new();

		for msg in &msg_chunk {
			let tx = Arc::clone(&tx);

			// TODO: break the laws of thermodynamics and try to remove this copy
			let msg = msg.to_owned();

			let handle = runtime.spawn(async move {
				let mut sender = tx.lock().await;

				sender.feed(msg).await
			});

			futures.push(handle);
		}

		println!("sending chunk {id} ({} futures)...", futures.len());

		let handle = runtime.spawn(async {
			join_all(futures).await;
		});

		let tx = Arc::clone(&tx);
		runtime.block_on(async move {
			handle.await.unwrap();

			println!("flushing chunk {id}...");

			let mut sender = tx.lock().await;
			sender.flush().await.unwrap();
		});

		if !(repeat) {
			break;
		}
	}
}

/// Pregenerate the websocket messages
fn generate_ws_messages(img_file: &str, alpha: Option<&u8>) -> Result<Vec<Message>, Error> {
	let img_reader = Reader::open(img_file)?;
	let img = img_reader.decode()?;

	let w = img.width();
	let h = img.height();

	let mut messages: Vec<Message> = Vec::with_capacity(w as usize * h as usize);

	for x in 0..w {
		for y in 0..h {
			let pixel = img.get_pixel(x, y).0;

			let x_bytes: [u8; 4] = x.to_be().to_ne_bytes();
			let y_bytes: [u8; 4] = y.to_be().to_ne_bytes();

			let bytes = vec![
				x_bytes[0],
				x_bytes[1],
				x_bytes[2],
				x_bytes[3],
				y_bytes[0],
				y_bytes[1],
				y_bytes[2],
				y_bytes[3],
				pixel[0],
				pixel[1],
				pixel[2],
				*alpha.unwrap_or_else(|| &pixel[3]),
			];

			messages.push(Message::Binary(bytes))
		}
	}

	messages.truncate(messages.len());

	Ok(messages)
}

fn send_ws(ws_url_raw: &str, img_file: &str, alpha: Option<&u8>, shuffle: bool, repeat: bool) {
	let ws_url = match ws_url_raw.parse::<Url>() {
		Ok(u) => u,
		Err(e) => {
			eprintln!("{:?}", e);
			process::exit(1);
		},
	};

	println!("creating WebSocket message array...");

	let mut messages = match generate_ws_messages(img_file, alpha) {
		Ok(m) => m,
		Err(e) => {
			eprintln!("{:?}", e);
			process::exit(1);
		},
	};

	if shuffle {
		println!("shuffling messages...");
		messages.shuffle(&mut thread_rng());
	}

	println!("generated {} messages", messages.len());

	let num_cpus = num_cpus::get();
	let chunk_size = messages.len() / num_cpus;

	println!("starting with {num_cpus} threads...");

	let mut handles = vec![];

	for (id, chunk) in messages.chunks(chunk_size).enumerate() {
		// Fucking borrow checker doesn't realise that these threads *don't* actually need
		// 'static lifetimes so i have clone this smhsmhsmhsmhsmh
		let url = ws_url.clone();
		let chunk = chunk.to_vec();

		let handle = thread::spawn(move || send_chunk_ws(id, chunk, &url, repeat));

		handles.push(handle);
	}

	for handle in handles {
		handle.join().unwrap();
	}
}
fn main() {
	let matches = Command::new(env!("CARGO_PKG_NAME"))
		.version(env!("CARGO_PKG_VERSION"))
		.author(env!("CARGO_PKG_AUTHORS"))
		.about(env!("CARGO_PKG_DESCRIPTION"))
		.arg_required_else_help(true)
		.arg(
			Arg::new("repeat")
				.short('r')
				.long("repeat")
				.help("Whether or not to keep uploading the image (battle mode)")
				.action(ArgAction::SetTrue),
		)
		.arg(
			Arg::new("shuffle")
				.short('s')
				.long("shuffle")
				.help("Whether or not to randomly shuffle the images content before uploading")
				.action(ArgAction::SetTrue),
		)
		.arg(
			Arg::new("alpha")
				.short('a')
				.long("alpha")
				.help("An optional alpha value for each pixel")
				.takes_value(true)
				.number_of_values(1)
				.value_parser(clap::value_parser!(u8)),
		)
		.arg(
			Arg::new("ws_url")
				.short('w')
				.long("ws")
				.help("The WebSocket URL to connect to")
				.takes_value(true)
				.number_of_values(1)
				.value_parser(clap::value_parser!(String))
				.required_unless_present(true),
		)
		.arg(Arg::new("img_file").help("The path of the image to upload").index(1).required(true))
		.get_matches();

	// Unwrap is safe as args are guaranteed to exist
	let ws_url = matches.get_one::<String>("ws_url").unwrap();

	let img_file = matches.get_one::<String>("img_file").unwrap();
	let alpha = matches.get_one::<u8>("alpha");

	let repeat = *matches.get_one::<bool>("repeat").unwrap();
	let shuffle = *matches.get_one::<bool>("shuffle").unwrap();

	send_ws(ws_url, img_file, alpha, shuffle, repeat)
}
