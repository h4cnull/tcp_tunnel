use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};
use tcp_tunnel::{load_client_config, xor, EncReader, EncWriter, TcpTunnelClientConfig, CLOSE_ID};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpStream, sync::Mutex};
use log::{info, error};

async fn client_handle(mut stream:TcpStream,config:(String,TcpTunnelClientConfig)) -> tokio::io::Result<()> {

    let tunnel_name = config.0;
    let config = config.1;

    let name_len = tunnel_name.as_bytes().len();
    let mut data = vec![name_len as u8];
    data.extend_from_slice(&tunnel_name.as_bytes());
    let remote_addr = config.remote_addr.to_string();
    let mut auth_data = vec![];
    xor(remote_addr.as_bytes(), config.key.as_bytes(), &mut auth_data);
    data.append(&mut auth_data);
    let _ = stream.write_all(&data).await?;
    info!("tunnel {} auth finished",tunnel_name);

    let (mut tunnel_reader,tunnel_writer) = stream.into_split();
    let tunnel_writer = Arc::new(Mutex::new(tunnel_writer));
    let mut handles: Vec<tokio::task::JoinHandle<()>> = vec![];

    let connections_writers: Arc<Mutex<HashMap<u32, tokio::net::tcp::OwnedWriteHalf>>> = Arc::new(Mutex::new(HashMap::new()));
    
    let key = config.key;

    let mut enc_reader = EncReader::new(key.clone());
    let mut enc_writer = EncWriter::new(key.clone());
    let mut len_bytes = [0;4];

    let mut write_data = vec![];
    let mut data  = vec![];
    loop {
        data.clear();
        let r = enc_reader.read_from_tunnel(&mut tunnel_reader, &mut data).await;
        if r.is_err() {
            error!("error while read from tunnel {} stream : {}",tunnel_name,r.err().unwrap());
            break;
        }

        for i in 0..4 {
            len_bytes[i] = data[i];
        }
        let id = u32::from_be_bytes(len_bytes);
        let data = &data[4..];
        
        if id != 0 {
            let mut l = connections_writers.lock().await;
            if id == CLOSE_ID {
                for i in 0..4 {
                    len_bytes[i] = data[i];
                }
                let id = u32::from_be_bytes(len_bytes);
                if let Some(mut s) = l.remove(&id) {
                    let _ = s.shutdown().await;
                    info!("tunnel {} close connection {}",tunnel_name,id);
                }
            } else {
                if let Some(s) = l.get_mut(&id) {
                    let mut err: bool = false;
                    let r = s.write_all(data).await;
                    err = r.is_err();
                    if err {
                        let mut s = l.remove(&id).unwrap();
                        let _ = s.shutdown().await;
                        let mut w = tunnel_writer.lock().await;
                        write_data.clear();
                        write_data.extend_from_slice(&CLOSE_ID.to_be_bytes());
                        write_data.extend_from_slice(&id.to_be_bytes());
                        let _ = enc_writer.write_to_tunnel(&mut w, &write_data);
                    }
                    drop(l);
                } else {
                    drop(l);
                    let addr = config.local_addr;
                    info!("tunnel {} new connection {} to {}",tunnel_name,id,addr);
                    let connections_to_tunnel_writer = tunnel_writer.clone();
                    let shared_connections_writers = connections_writers.clone();
                    let data_from_tunnel = data.to_vec();
                    let connections_tunnel_name = tunnel_name.clone();
                    let key = key.clone();
                    let h = tokio::spawn(async move {
                        let s = TcpStream::connect(addr).await;
                        let mut write_data = vec![];
                        let mut enc_writer = EncWriter::new(key);
                        match s {
                            Ok(mut stream) => {
                                let mut l = shared_connections_writers.lock().await;
                                let _ = stream.write_all(&data_from_tunnel).await;
                                let (mut reader,writer) = stream.into_split();
                                l.insert(id, writer);
                                drop(l);
                                let mut buf = [0;4096];
                                loop {
                                    let r = reader.read(&mut buf).await;
                                    write_data.clear();
                                    let mut w = connections_to_tunnel_writer.lock().await;
                                    match r {
                                        Ok(n) => {
                                            if n == 0 {
                                                error!("tunnel {} connection {} read date from {} data length 0",connections_tunnel_name,id,addr);
                                                write_data.extend_from_slice(&CLOSE_ID.to_be_bytes());
                                                write_data.extend_from_slice(&id.to_be_bytes());
                                                let _ = enc_writer.write_to_tunnel(&mut w, &write_data).await;
                                                break;
                                            }
                                            info!("tunnel {} connection {} write {} bytes data to tunnel",connections_tunnel_name,id,n);
                                            write_data.extend_from_slice(&id.to_be_bytes());
                                            write_data.extend_from_slice(&buf[..n]);
                                            let _ = enc_writer.write_to_tunnel(&mut w, &write_data).await;
                                        },
                                        Err(e) => {
                                            error!("tunnel {} connection {} read data from {} error {}",connections_tunnel_name,id,addr,e);
                                            write_data.extend_from_slice(&CLOSE_ID.to_be_bytes());
                                            write_data.extend_from_slice(&id.to_be_bytes());
                                            let _ = enc_writer.write_to_tunnel(&mut w, &write_data).await;
                                            break;
                                        }
                                    }
                                    drop(w);
                                }
    
                            },
                            Err(e) => {
                                let mut w = connections_to_tunnel_writer.lock().await;
                                write_data.clear();
                                write_data.extend_from_slice(&CLOSE_ID.to_be_bytes());
                                write_data.extend_from_slice(&id.to_be_bytes());
                                let _ = enc_writer.write_to_tunnel(&mut w, &write_data).await;
                                error!("tunnel {} connection {} connect to {} error {}",connections_tunnel_name,id,addr,e);
                            }
                        }
                    });
                    handles.push(h);
                };
            }
        }
    }
    for h in handles.iter() {
        h.abort();
    }
    info!("tunnel {} closed",tunnel_name);
    Ok(())
}

async fn client(server_addr: SocketAddr, reconn: u64, config: (String, TcpTunnelClientConfig)) {
    let tunnel_name = config.0.clone();
    loop {
        let s = TcpStream::connect(server_addr).await;
        match s {
            Ok(stream) => {
                let _r = client_handle(stream,config.clone()).await;

            },
            Err(e) => {
                error!("tunnel {} connect to {} {}",tunnel_name,server_addr,e);
            }
        }
        tokio::time::sleep(Duration::from_secs(reconn)).await;
        info!("tunnel {} reconnecting to server {}",tunnel_name,server_addr);
    }
}

#[tokio::main]
async fn main() {
    #[cfg(target_arch="x86_64")]
    env_logger::init();
    let args:Vec<String> = std::env::args().collect();
    let config = load_client_config(&args[1]);
    let mut handles = vec![];
    for (k,v) in config.tunnel.iter() {
        let addr = config.server_addr;
        handles.push(tokio::spawn(client(addr, config.reconn, (k.clone(),v.clone()))));
    }
    for h in handles {
        let _ = h.await;
    }
}
