use std::{collections::HashMap, net::{Ipv4Addr, SocketAddr, SocketAddrV4}, sync::Arc, time::Duration};
use tcp_tunnel::{load_server_config, xor, EncReader, EncWriter, TcpTunnelServerConfig, CLOSE_ID, CONNECTION_ID_START, PING_ID};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}, sync::Mutex};
use log::{info, error, debug};

async fn server_handle(mut tunnel_stream: TcpStream, tunnel_confs:Arc<HashMap<String,TcpTunnelServerConfig>>) {
    let mut buf = [0u8; 1024];
    // 读取客户端发来的认证信息并进行验证
    let n = match tunnel_stream.read(&mut buf).await {
        Ok(0) => {
            error!("tunnel client closed the connection");
            return;
        }
        Ok(n) => n,
        Err(e) => {
            error!("failed to read from tunnel stream: {}", e);
            return;
        }
    };
    let buffer = &buf[..n];
    debug!("auth data {:?}",buffer);
    let len = buffer[0] as usize;
    if len + 1 > buffer.len() {
        error!("unknown tunnel connection");
        return;
    }

    let tunnel_name = String::from_utf8_lossy(&buffer[1..1+len]);
    let tunnel_name:&str = &tunnel_name;

    let conf = tunnel_confs.get(tunnel_name);
    if conf.is_none() {
        error!("not found tunnel config {:?}",tunnel_name);
        return;
    }
    
    let conf = conf.unwrap().clone();
    let mut output = vec![];

    xor(&buffer[1+len..], conf.key.as_bytes(), &mut output);
    
    let addr = String::from_utf8_lossy(&output);

    let listen_addr = addr.parse::<SocketAddr>();
    if listen_addr.is_err() {
        error!("tunnel {} unknown listen addr {:?}",tunnel_name,addr);
        return;
    }
    

    let listen_addr = listen_addr.unwrap();

    info!("tunnel {} authentication succeeded",tunnel_name);

    let r = TcpListener::bind(listen_addr).await;
    if r.is_err() {
        error!("tunnel {} error listen at addr {:?}: {:?}",tunnel_name,addr,r);
        return;
    }
    let listen_stream = r.unwrap();
    info!("tunnel {} service listening on address: {}", tunnel_name, listen_addr);



    let (mut tunnel_reader,tunnel_writer) = tunnel_stream.into_split();

    // 多个客户端连接会公用一个tunnel_writer，每个客户端连接单独启动一个任务，当从客户端读取到数据时，向tunnel写入数据。
    let tunnel_writer = Arc::new(Mutex::new(tunnel_writer));
    // tunnel 需要能获取到客户端连接，当从tunnel读取到数据时，根据连接id向客户端发送数据。
    let client_writers: Arc<Mutex<HashMap<u32, tokio::net::tcp::OwnedWriteHalf>>> = Arc::new(Mutex::new(HashMap::new()));
    
    let client_writers_table = client_writers.clone();
    let tunnel_name = tunnel_name.to_string();

    let auth_key = conf.key.clone();

    let connections_to_tunnel_writer = tunnel_writer.clone();
    let connections_to_tunnel_key = auth_key.clone();
    let connections_to_tunnel_tname = tunnel_name.clone();

    // 启动一个任务，用于接受客户端的连接，每新建一个连接，创建一个task，共用一个tunnel_writer

    let handles = Arc::new(Mutex::new(vec![]));
    let shared_handles = handles.clone();
    let connections_to_tunnel_h = tokio::spawn(async move {
        let mut id = CONNECTION_ID_START - 1;
        while let Ok((stream,addr)) = listen_stream.accept().await {
            id += 1;
            let (mut reader,writer) = stream.into_split();
            // 将writer存入client_writers中
            let mut l: tokio::sync::MutexGuard<'_, HashMap<u32, tokio::net::tcp::OwnedWriteHalf>> = client_writers_table.lock().await;
            l.insert(id, writer);
            let writer = connections_to_tunnel_writer.clone();
            let key = connections_to_tunnel_key.clone();
            let tunnel_name = connections_to_tunnel_tname.clone();
            let h = tokio::spawn(async move {
                let mut buf = [0;4096];
                
                let mut write_data = vec![];
                let mut enc_writer = EncWriter::new(key);

                loop {
                    let r = reader.read(&mut buf).await;
                    let mut w = writer.lock().await;
                    write_data.clear();
                    match r {
                        Ok(n) => {
                            debug!("tunnel {} connection {} read data from client {}",tunnel_name,id,String::from_utf8_lossy(&buf[..n]));
                            if n == 0 {
                                error!("tunnel {} connection {} read data 0, send close to tunnel",tunnel_name,id);
                                write_data.extend_from_slice(&CLOSE_ID.to_be_bytes());
                                write_data.extend_from_slice(&id.to_be_bytes());
                                let r = enc_writer.write_to_tunnel(&mut w, &write_data).await;
                                break;
                            }
                            write_data.extend_from_slice(&id.to_be_bytes());
                            write_data.extend_from_slice(&buf[..n]);
                            let r = enc_writer.write_to_tunnel(&mut w, &write_data).await;
                            if r.is_err() {
                                error!("tunnel {} connection {} read data written to tunnel error: {:?}",tunnel_name,id,r);
                                break;
                            }
                            info!("tunnel {} connection {} read data written to tunnel ok",tunnel_name,id);
                        },
                        Err(e) => {
                            error!("tunnel {} connection {} error while read from client {}: {}",tunnel_name,id,addr,e);
                            write_data.extend_from_slice(&CLOSE_ID.to_be_bytes());
                            write_data.extend_from_slice(&id.to_be_bytes());
                            let r = enc_writer.write_to_tunnel(&mut w, &write_data).await;
                            break;
                        }
                    }
                }
                info!("tunnel {} connection {} finished",tunnel_name,id);
            });
            let mut handles = shared_handles.lock().await;
            handles.push(h);
        }
    });
    
    let tunnle_to_connections_tunnel_name = tunnel_name.clone();
    
    let tunnle_to_connections_key = auth_key.clone();
    let connections_to_tunnel_h = Arc::new(connections_to_tunnel_h);
    let listener_h = connections_to_tunnel_h.clone();
    let tunnle_to_connections_key_h = tokio::spawn(async move {
        let mut enc_reader = EncReader::new(tunnle_to_connections_key);
        let mut data = vec![];
        let mut len_bytes = [0;4];
        loop {
            data.clear();
            let r = enc_reader.read_from_tunnel(&mut tunnel_reader, &mut data).await;
            match r {
                Ok(_) => {
                    for i in 0..4 {
                        len_bytes[i] = data[i];
                    }

                    let id = u32::from_be_bytes(len_bytes);
                    let data = &data[4..];

                    info!("tunnel {} connection {} write {} bytes data to client",tunnle_to_connections_tunnel_name,id,data.len());
                    let mut l = client_writers.lock().await;
                    if id == CLOSE_ID {
                        for i in 0..4 {
                            len_bytes[i] = data[i];
                        }
                        let id = u32::from_be_bytes(len_bytes);
                        if let Some(mut s) = l.remove(&id) {
                            let _ = s.shutdown().await;
                            info!("tunnel {} connection {} closed",tunnle_to_connections_tunnel_name,id);
                        } else {
                            error!("receive close request from tunnel {}, but not found client connection {}",tunnle_to_connections_tunnel_name,id);
                        }
                    } else {
                        let mut error = false;
                        if let Some(s) = l.get_mut(&id) {
                            let r = s.write_all(data).await;
                            info!("tunnel {} connection {} write data to client {:?}",tunnle_to_connections_tunnel_name,id,r);
                            error = r.is_err();
                        } else {
                            error!("receive data from tunnel {}, but not found client connection {}, this pkt will be dropped",tunnle_to_connections_tunnel_name,id);
                        };
                        if error {
                            if let Some(mut s) = l.remove(&id) {
                                let _ = s.shutdown().await;
                            };
                        }
                    }
                    drop(l);
                },
                Err(e) => {
                    error!("error while read from tunnel {} stream : {}, tunnel closed!",tunnle_to_connections_tunnel_name,e);
                    listener_h.abort();
                    break;
                }
            }
        }
    });

    let mut write_data = vec![];
    let mut enc_writer = EncWriter::new(auth_key);
    loop {
        tokio::time::sleep(Duration::from_secs(20)).await;
        let mut w = tunnel_writer.lock().await;
        write_data.clear();
        write_data.extend_from_slice(&PING_ID.to_be_bytes());
        write_data.extend_from_slice("PING".as_bytes());
        let r = enc_writer.write_to_tunnel(&mut w, &write_data).await;
        if r.is_err() {
            break;
        }
        info!("ping client success");
    }
    info!("tunnel {} closed",tunnel_name);
    connections_to_tunnel_h.abort();
    let handles = handles.lock().await;
    for h in handles.iter() {
        h.abort();
    }
    tunnle_to_connections_key_h.abort();
}

async fn server(addr: SocketAddr, config: HashMap<String, TcpTunnelServerConfig>) {
    let listener = TcpListener::bind(addr).await.unwrap();
    info!("server listening on {}", addr);
    let config = Arc::new(config);
    while let Ok((client_stream, _)) = listener.accept().await {
        let conf = config.clone();
        tokio::spawn(server_handle(client_stream, conf));
    }
}

#[tokio::main]
async fn main() {
    #[cfg(target_arch="x86_64")]
    env_logger::init();
    let args:Vec<String> = std::env::args().collect();
    let config = load_server_config(&args[1]);
    let listen_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0),config.listen_port));
    server(listen_addr, config.tunnel).await;
}
