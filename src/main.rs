mod knock_meta;
mod knock_state;

use ahash::{HashMap, HashMapExt};
use knock_meta::KnockMeta;
use knock_state::KnockState;
use quanta::{Instant, Upkeep};
use slotmap::{DefaultKey, SlotMap};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::hash_map::Entry;
use std::collections::BTreeMap;
use std::io::Read;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::time::Duration;
use std::{io, mem};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncReadExt, Interest, ReadBuf};
use tokio::time::interval;
use tokio::{pin, select};
use tracing::{info, trace, Level};

type KnockKey = DefaultKey;

struct RawSocket {
    inner: AsyncFd<Socket>,
}

impl RawSocket {
    fn new() -> tokio::io::Result<Self> {
        let sock = Socket::new_raw(Domain::PACKET, Type::RAW, Some(Protocol::from(768)))?;
        sock.set_nonblocking(true)?;
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

// TODO: Move to tokio current_thread executor - no locks needed if we do and allows for easy time
//       slicing between cleanup and packet processing
// TODO: Switch to either nix crate or socket2 to create raw socket to feed into tokio's AsyncFd
// TODO: Either use etherparse or libpnet_packet for parsing - etherparse is zero-copy
fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(Level::INFO)
        .without_time()
        .init();

    let _upkeep = Upkeep::new(Duration::from_millis(5)).start().unwrap();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(run());
}

async fn run() {
    let mut knock_states = slotmap::SlotMap::<KnockKey, KnockState>::new();
    let mut active_knocks = HashMap::<IpAddr, KnockKey>::new();
    let mut expiration_queue = BTreeMap::<Instant, Vec<(IpAddr, KnockKey)>>::new();
    let mut buf = [0u8; 256];

    let mut socket = RawSocket::new().unwrap();
    let cleanup_interval = interval(Duration::from_secs(5));
    pin!(cleanup_interval);

    loop {
        select! {
            biased;
            count = socket.read(&mut buf) => {
                let count = count.expect("socket is dead");
                handle_knock(
                    &buf[..count],
                    &mut knock_states,
                    &mut active_knocks,
                    &mut expiration_queue,
                );
            },
            _ = cleanup_interval.tick() => {
                handle_cleanup(
                    &mut knock_states,
                    &mut active_knocks,
                    &mut expiration_queue
                );
            }
        }
    }
}

fn handle_knock(
    packet: &[u8],
    knock_states: &mut SlotMap<DefaultKey, KnockState>,
    active_knocks: &mut HashMap<IpAddr, KnockKey>,
    expiration_queue: &mut BTreeMap<Instant, Vec<(IpAddr, KnockKey)>>,
) {
    let Ok(KnockMeta {
        dst_addr, dst_port, ..
    }) = KnockMeta::try_from(packet)
    else {
        return;
    };

    let span = tracing::info_span!("packet", %dst_addr, %dst_port);
    let _entered = span.enter();

    let knock_entry = active_knocks.entry(dst_addr);

    let knock = match &knock_entry {
        Entry::Vacant(_) => KnockState::try_new(dst_port),
        Entry::Occupied(e) => {
            if let KnockState::Passed { .. } = knock_states[e.get().clone()] {
                trace!("Backdoor already open");
                return;
            }

            knock_states
                .remove(e.get().clone())
                .unwrap()
                .progress(dst_port)
        }
    };

    if !knock.is_valid() {
        if let Entry::Occupied(e) = knock_entry {
            info!("Quiet inside!");
            e.remove();
        }
        return;
    }

    let expiration = knock.expiration();
    let knock_key = knock_states.insert(knock);

    match expiration_queue
        .last_entry()
        .filter(|e| e.key() == &expiration)
    {
        Some(mut e) => {
            e.get_mut().push((dst_addr, knock_key));
        }
        None => {
            expiration_queue.insert(expiration, vec![(dst_addr, knock_key)]);
        }
    };

    match knock_entry {
        Entry::Occupied(mut e) => {
            e.insert(knock_key);
        }
        Entry::Vacant(e) => {
            e.insert(knock_key);
        }
    }

    match knock {
        KnockState::PortPending { .. } => {
            info!("Knock...");
        }
        KnockState::Passed { .. } => {
            info!("Backdoor opened");
        }
        KnockState::Failed => unreachable!("This is never valid by this point"),
    }
}

fn handle_cleanup(
    knock_states: &mut SlotMap<DefaultKey, KnockState>,
    active_knocks: &mut HashMap<IpAddr, KnockKey>,
    expiration_queue: &mut BTreeMap<Instant, Vec<(IpAddr, KnockKey)>>,
) {
    // Cannot use the one from cleanup interval, as we are using quanta's version for performance
    let now = Instant::recent();
    // This returns the second half initially, we want the opposite
    let mut expired_items = expiration_queue.split_off(&now);
    mem::swap(expiration_queue, &mut expired_items);

    for (addr, key) in expired_items.into_values().flatten() {
        if let Some(knock) = knock_states.remove(key) {
            if matches!(knock, KnockState::Passed { .. }) {
                info!("Backdoor closed for {addr}");
            } else {
                info!("Cleared knock attempts for {addr}");
            }
            active_knocks.remove(&addr);
        }
    }
}
