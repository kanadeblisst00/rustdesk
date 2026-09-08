use rustdesk_agent_mcp::http;

#[tokio::test]
async fn listener_releases_port_after_accepting_a_connection() {
    let listener = http::bind("0.0.0.0:0".parse().unwrap()).await.unwrap();
    let address = listener.local_addr().unwrap();
    let client = tokio::net::TcpStream::connect(("127.0.0.1", address.port()))
        .await
        .unwrap();
    let (accepted, _) = listener.accept().await.unwrap();
    drop(client);
    drop(accepted);
    drop(listener);
    let replacement = http::bind(address.to_string().parse().unwrap())
        .await
        .unwrap();
    assert_eq!(replacement.local_addr().unwrap(), address);
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{ffi::c_void, os::windows::io::AsRawSocket};

    #[link(name = "kernel32")]
    extern "system" {
        fn GetHandleInformation(handle: *mut c_void, flags: *mut u32) -> i32;
    }

    fn assert_not_inheritable(socket: &impl AsRawSocket) {
        let mut flags = 0;
        assert_ne!(
            unsafe { GetHandleInformation(socket.as_raw_socket() as *mut c_void, &mut flags) },
            0,
            "{}",
            std::io::Error::last_os_error()
        );
        assert_eq!(
            flags & 1,
            0,
            "MCP socket must not escape into a child process"
        );
    }

    #[tokio::test]
    async fn listener_and_accepted_socket_are_not_inheritable() {
        let listener = http::bind("0.0.0.0:0".parse().unwrap()).await.unwrap();
        assert_not_inheritable(&listener);
        let _client =
            tokio::net::TcpStream::connect(("127.0.0.1", listener.local_addr().unwrap().port()))
                .await
                .unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        assert_not_inheritable(&accepted);
    }
}
