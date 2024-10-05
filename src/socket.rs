use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::io::Read;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, Interest, ReadBuf};

pub struct RawSocket {
    inner: AsyncFd<Socket>,
}

impl RawSocket {
    pub fn new() -> io::Result<Self> {
        let sock = Socket::new_raw(Domain::PACKET, Type::RAW, Some(Protocol::from(768)))?;
        sock.set_nonblocking(true)?;
        let inner = AsyncFd::with_interest(sock, Interest::READABLE)?;
        Ok(Self { inner })
    }

    /// Wait until the socket is readable.
    ///
    /// This is not really used in any actual logic for this purpose - it's primarily used to notify
    /// when to cancel background jobs.
    pub async fn readable(&self) {
        self.inner
            .readable()
            .await
            .expect("socket shouldn't be dead")
            .retain_ready();
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
            match guard.try_io(|inner| inner.get_ref().read(unfilled)) {
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
