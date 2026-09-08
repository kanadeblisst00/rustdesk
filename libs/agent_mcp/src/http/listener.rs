use std::net::SocketAddrV4;

pub async fn bind(address: SocketAddrV4) -> std::io::Result<tokio::net::TcpListener> {
    #[cfg(windows)]
    {
        // Mio 1.0.3 creates inheritable Windows sockets. Create the handle with
        // std (WSA_FLAG_NO_HANDLE_INHERIT) before exposing it to concurrent spawns.
        let listener = std::net::TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        tokio::net::TcpListener::from_std(listener)
    }
    #[cfg(not(windows))]
    tokio::net::TcpListener::bind(address).await
}
