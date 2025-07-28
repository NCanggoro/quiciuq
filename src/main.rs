use std::{collections::HashMap, env, net::SocketAddr, path::PathBuf, sync::Arc, time::Instant};
use anyhow::{{anyhow, bail, Result}};
use bytes::{Bytes, BytesMut};
use futures::AsyncWriteExt;
use s2n_quic::{client::Connect, Client, Server};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

#[derive(Deserialize, Serialize, Debug, PartialEq, Clone)]
struct Request {
	id: u64,
	name: String
}
#[derive(Deserialize, Serialize, Debug, PartialEq, Clone)]
struct Response<'a> {
	id: u64,
	#[serde(borrow)]
	data: Res<'a>
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Clone)]
enum Res<'a> {
	NotFound,
	Found { data: &'a [u8] }
}

impl Res<'_> {
	pub fn len(&self) -> usize {
		match self {
			Res::NotFound => 0,
			Res::Found { data} => data.len()
		}
	}
}

async fn main_client(name: &str) -> Result<()> {
	let keypair = libp2p_identity::Keypair::generate_ed25519();
	let client_config = libp2p_tls::make_client_config(&keypair, None)?;
	let tls = s2n_quic::provider::tls::rustls::Client::from(client_config);
	let client = Client::builder()
		.with_tls(tls)?
		.with_io("0.0.0.0:0")?
		.start()
		.map_err(|e| anyhow!("{:?}", e))?;

	let addr: SocketAddr = "127.0.0.1:4433".parse()?;
	let connect = Connect::new(addr).with_server_name("localhost");
	let mut connection = client.connect(connect).await?;

	connection.keep_alive(true)?;

	let stream = connection.open_bidirectional_stream().await?;
	let (mut reader, mut writer) = 	stream.split();

	let req = Request {
		id: 1,
		name: name.to_string()
	};

	let mut bytes = BytesMut::zeroed(15 + name.len());
	let used = postcard::to_slice(&req, &mut bytes)?;

	let mut buffer = [0u8; 10];
	let lp = unsigned_varint::encode::u64(used.len() as u64, &mut buffer);
	writer.write_all(lp).await?;

	writer.write_all(used).await?;
	let now = Instant::now();


	{
		let mut in_buffer = BytesMut::zeroed(1024);

		let size = unsigned_varint::aio::read_u64(&mut reader).await.unwrap();

		in_buffer.clear();
		while (in_buffer.len() as u64) < size {
			reader.read_buf(&mut in_buffer).await.unwrap();
		}

		let size = usize::try_from(size).unwrap();
		let response: Response = postcard::from_bytes(&in_buffer[..size])?;

		match response.data {
			Res::Found { data } => {
				println!("response size: {}", data.len());
			}
			Res::NotFound => {
				bail!("data not found")
			}
		}
	}

	println!("elapsed {}ms", now.elapsed().as_millis());

	Ok(())
}


async fn main_server() -> Result<()> {
	let kp = libp2p_identity::Keypair::generate_ed25519();
	let server_config = libp2p_tls::make_server_config(&kp)?;
	let tls = s2n_quic::provider::tls::rustls::Server::from(server_config);

	let limits = s2n_quic::provider::limits::Default::default();

	let db: Arc<HashMap<String, Vec<u8>>> = Arc::new(
		[
			("1MB".to_string(), vec![1u8; 1024*1024*1]),
			("10MB".to_string(), vec![1u8; 1024*1024*10]),
			("100MB".to_string(), vec![1u8; 1024*1024*100])
		]
		.into_iter()
		.collect()
	);

	let mut server = Server::builder()
		.with_tls(tls)?
		.with_io("127.0.0.1:4433")?
		.with_limits(limits)?
		.start()
		.map_err(|e| anyhow!("{:?}", e))?;

	while let Some(mut connection) = server.accept().await {
		let db = db.clone();
		tokio::spawn(async move {
			eprintln!("Connection accepted from {:?}", connection.remote_addr());

			while let Ok(Some(mut stream)) = connection.accept_bidirectional_stream().await {
				let db = db.clone();
				tokio::spawn(async move {
					println!("Stream opened from {:?}", stream.connection().remote_addr());
					let (mut reader, mut writer) = stream.split();
					let mut out_buffer = BytesMut::zeroed(1024);
					let mut in_buffer = BytesMut::zeroed(1024);
					let mut lp_buffer = [0u8; 10];

					while let Ok(size) = unsigned_varint::aio::read_u64(&mut reader).await {
						in_buffer.clear();

						while (in_buffer.len() as u64) < size {
							reader.read_buf(&mut in_buffer).await.unwrap();
						}
						let size = usize::try_from(size).unwrap();
						let request: Request = postcard::from_bytes(&in_buffer[..size]).unwrap();
						let data = if let Some(data) = db.get(&request.name) {
							Res::Found { data: &data[..] }
						} else {
							Res::NotFound
						};

						let response = Response {
							id: request.id,
							data
						};

						out_buffer.clear();
						if out_buffer.len() < 20 + response.data.len() {
							out_buffer.resize(20 + response.data.len(), 0u8);
						}
						let used = postcard::to_slice(&response, &mut out_buffer).unwrap();
						let lp = unsigned_varint::encode::u64(used.len() as u64, &mut lp_buffer);
						writer.write_all(lp).await.unwrap();

						if let Err(e) = writer.write_all(used).await {
							eprintln!("failed to write response: {:?}", e);
						}
					}

					println!("Disconnected");
				});
			}
		});
	}


	Ok(())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
	let args: Vec<String> = env::args().collect();

	if args.len() < 2 {
		bail!("invalid argument");
	}

	println!("{:?}", args);

	// Clap 
	match args[1].as_str() {
		"client" => {
			let name = &args[2];
			println!("sending {:?}", name);
			main_client(name).await?;
		}
		"server" => main_server().await?,
		_ => bail!("unknown argument: {}", &args[0])
	}

	Ok(())


}