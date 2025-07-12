

    tcp_tunnel 是一个非常简单的tcp隧道程序，支持简单的xor混淆，写这个程序的目的是为了管理内网非常“丐”版的mips路由器，我试过xfrp和ngrok-c，体积还是大，索性写了这个简单的隧道程序，编译后体积很小，只有717K，upx压缩后351K。运行和配置非常简单。

server端配置server.toml：

```toml
listen_port = 7000

[tunnel.tcp1]
key = "123456"

[tunnel.tcp2]
key = "654321"
```

server端运行：

```bash
RUST_LOG=info ./server server.toml
```



client端配置client.toml：

```toml
server_addr = "123.123.123.123:7000"
reconn = 30

[tunnel.tcp1]
local_addr = "192.168.1.1:22"
remote_addr = "0.0.0.0:2222"
```

client端运行：

```bash
./client client.toml
```

这样就能将内网192.168.1.1:22 通过隧道转发到公网123.123.123.123的2222端口。


**编译，只介绍主要的步骤。**

编译系统环境是centos7.9，rustc 1.89.0-nightly。

**linux-musl**

rust可以直接添加x86_64-unknown-linux-musl target

```
rustup target add x86_64-unknown-linux-musl
```

编译

```bash
cargo build --bin server --release --target x86_64-unknown-linux-musl
cargo build --bin client --release --target x86_64-unknown-linux-musl
```

**windows-gnu**

要支持windows7，windows server 2008 r2旧版系统，编译相对繁琐，需先安装mingw-w64 ，centos 7.9默认的是gcc 4.8，不支持编译mingw-w64工具链，需要先yum安装devtoolset-11，然后用gcc-11编译:

```bash

git clone https://github.com/Zeranoe/mingw-w64-build/
./mingw-w64-build --keep-artifacts --gcc-branch releases/gcc-12 x86_64     #中途失败，添加--cached-sources参数重新执行
cp /root/.zeranoe/mingw-w64/bld/x86_64/mingw-w64-winpthreads/fakelib/libgcc_eh.a /root/.zeranoe/mingw-w64/x86_64/lib/
export PATH=$PATH:/root/.zeranoe/mingw-w64/x86_64/bin/
```

然后编译。

```bash
cargo build -Zbuild-std=std,panic_abort --bin server --release --target x86_64-win7-windows-gnu
cargo build -Zbuild-std=std,panic_abort --bin client --release --target x86_64-win7-windows-gnu
```

mipsel-musl

编译起来很繁琐，需要用buildroot编译mips little endian的gcc，然后编译相应的musl-gcc，还有libunwind，最后才能指定CC编译。注意查看.cargo/config.toml中的配置，最终编译命令如下：

```
CC=/mipsel-linux/musl/mipsel-musl-gcc cargo build -Z build-std=std,panic_abort --bin server --release --target mipsel-unknown-linux-musl
CC=/mipsel-linux/musl/mipsel-musl-gcc cargo build -Z build-std=std,panic_abort --bin client --release --target mipsel-unknown-linux-musl
```

如果这个项目帮助到了你，点一下小⭐⭐吧。
