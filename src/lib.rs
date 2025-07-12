use std::{collections::HashMap, net::SocketAddr};

use serde::Deserialize;
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{tcp::{OwnedReadHalf, OwnedWriteHalf}, TcpListener}};

#[derive(Deserialize)]
pub struct ServerConfig {
    pub listen_port: u16,
    pub tunnel: HashMap<String,TcpTunnelServerConfig>
}

#[derive(Deserialize,Clone)]
pub struct TcpTunnelServerConfig {
    pub key: String,
}

pub static PING_ID:u32 = 0;
pub static CLOSE_ID:u32 = 1;
pub static CONNECTION_ID_START: u32 = 10;

pub fn load_server_config(file_path: &str) -> ServerConfig {
    let config_str = std::fs::read_to_string(file_path).expect("Unable to read config file");
    let config: ServerConfig = toml::from_str(&config_str).unwrap();
    config
}

#[derive(Deserialize)]
pub struct ClientConfig {
    pub server_addr: SocketAddr,
    pub reconn: u64,
    pub tunnel: HashMap<String,TcpTunnelClientConfig>
}

#[derive(Deserialize,Clone)]
pub struct TcpTunnelClientConfig {
    pub remote_addr: SocketAddr,
    pub local_addr: SocketAddr,
    pub key: String,
}

pub fn load_client_config(file_path: &str) -> ClientConfig {
    let config_str = std::fs::read_to_string(file_path).expect("Unable to read config file");
    let config: ClientConfig = toml::from_str(&config_str).unwrap();
    config
}

pub fn xor(data:&[u8],key:&[u8],output:&mut Vec<u8>) {
    output.reserve(data.len());
    for i in 0..data.len() {
        let v = data[i] ^ key[i % key.len()];
        output.push(v);
    }
}

pub struct EncWriter {
    key:String,
    pkt_cache:Vec<u8>
}

impl EncWriter {
    pub fn new(key:String) -> Self {
        EncWriter {
            key,
            pkt_cache:vec![]
        }
    }

    pub async fn write_to_tunnel(&mut self,tunnel_writer:&mut OwnedWriteHalf,data:&[u8]) -> tokio::io::Result<usize> {
        self.pkt_cache.clear();
        xor(data, self.key.as_bytes(), &mut self.pkt_cache);
        tunnel_writer.write_u32(self.pkt_cache.len() as u32).await?;
        let r = tunnel_writer.write(&self.pkt_cache).await?;
        Ok(r + 4)
    }
}

pub struct EncReader {
    key:String,
    buf:[u8;4096],
    len_bytes:[u8;4],
    pkt_len: usize,
    pkt_data_len: u32,
    pkt_cache:Vec<u8>,
    next_pkt_cache:Vec<u8>,
    // output_cahce:Vec<u8>,
    need_read:bool
}

impl EncReader {
    pub fn new(key:String) -> Self {
        EncReader {
            key,
            buf:[0;4096],
            len_bytes:[0;4],
            pkt_len: 0,
            pkt_data_len: 0,
            pkt_cache:vec![],
            next_pkt_cache:vec![],
            // output_cahce:vec![],
            need_read: true
        }
    }

    pub async fn read_from_tunnel(&mut self,tunnel_reader:&mut OwnedReadHalf,data:&mut Vec<u8>) -> tokio::io::Result<()> {
        loop {
            if self.need_read {
                let n = tunnel_reader.read(&mut self.buf).await?;  
                if n == 0 {
                    return Err(tokio::io::Error::new(tokio::io::ErrorKind::ConnectionReset, "ConnectionReset"));
                }
                self.pkt_cache.extend_from_slice(&self.buf[..n]);
            }
            if self.pkt_data_len != 0 {
                // 已读取完成至少一个pkt
                if self.pkt_cache.len() >= self.pkt_len {
                    // 解析pkt，发送到writer
                    // self.output_cahce.clear();
                    xor(&self.pkt_cache[4..self.pkt_len], self.key.as_bytes(), data);
                    
                    assert!(data.len() > 4);

                    // for i in 0..4 {
                    //     self.len_bytes[i] = self.da[i];
                    // }

                    // let id = u32::from_be_bytes(self.len_bytes);
                    // data.extend_from_slice(&self.output_cahce[4..]);
                    
                    self.next_pkt_cache.clear();
                    let next = &self.pkt_cache[self.pkt_len..];
                    self.next_pkt_cache.extend_from_slice(next);
                    // 检查下一个包，如果长度大于4，可能是一个完整的包
                    if self.next_pkt_cache.len() > 4 {
                        for i in 0..4 {
                            self.len_bytes[i] = self.next_pkt_cache[i];
                        }
                        let len = u32::from_be_bytes(self.len_bytes);
                        // 说明不是一个完整的包，退出循环，继续读取
                        if len as usize + 4 > self.next_pkt_cache.len() {
                            std::mem::swap(&mut self.pkt_cache,&mut self.next_pkt_cache);
                            //(self.pkt_cache,self.next_pkt_cache) = (self.next_pkt_cache,self.pkt_cache);
                            self.pkt_data_len = len;
                            self.pkt_len = self.pkt_data_len as usize + 4;
                            self.need_read = true;
                            return Ok(());
                        } else {
                            std::mem::swap(&mut self.pkt_cache,&mut self.next_pkt_cache);
                            // (self.pkt_cache,self.next_pkt_cache) = (self.next_pkt_cache,self.pkt_cache);
                            self.pkt_data_len = len;
                            self.pkt_len = self.pkt_data_len as usize + 4;
                            self.need_read = false;
                            return Ok(());
                        }
                    } else {
                        // 否则还不知道下一个包的长度
                        std::mem::swap(&mut self.pkt_cache,&mut self.next_pkt_cache);
                        // (self.pkt_cache,self.next_pkt_cache) = (self.next_pkt_cache,self.pkt_cache);
                        self.pkt_data_len = 0;
                        self.pkt_len = 0;
                        self.need_read = true;
                        return Ok(());
                    }
                } else {
                    self.need_read = true;
                }
            } else {
                assert!(self.pkt_cache.len() >= 4);
                for i in 0..4 {
                    self.len_bytes[i] = self.pkt_cache[i];
                }
                self.pkt_data_len = u32::from_be_bytes(self.len_bytes);
                assert!(self.pkt_data_len != 0);
                self.pkt_len = self.pkt_data_len as usize + 4;
                self.need_read = self.pkt_len > self.pkt_cache.len();
            }
        }
    }
}