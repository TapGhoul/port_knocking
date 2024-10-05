use nix::sys::socket::{recv, socket, AddressFamily, MsgFlags, SockFlag, SockProtocol, SockType};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, Interest, ReadBuf};

pub struct RawSocket {
    inner: AsyncFd<OwnedFd>,
}

impl RawSocket {
    pub fn new() -> io::Result<Self> {
        let sock = socket(
            AddressFamily::Packet,
            SockType::Raw,
            SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK,
            SockProtocol::EthAll,
        )?;
        let inner = AsyncFd::with_interest(sock, Interest::READABLE)?;
        Ok(Self { inner })
    }
}

impl AsyncRead for RawSocket {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.inner.poll_read_ready(cx))?;

            let unfilled = buf.initialize_unfilled();

            let read_result = guard.try_io(|inner| {
                recv(inner.as_raw_fd(), unfilled, MsgFlags::empty()).map_err(|e| e.into())
            });

            match read_result {
                Ok(Ok(len)) => {
                    buf.advance(len);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(err)) => return Poll::Ready(Err(err)),
                Err(_would_block) => continue,
            }
        }
    }
}
