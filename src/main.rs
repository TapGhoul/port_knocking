mod knock_meta;
mod knock_state;
mod socket;

use ahash::{HashMap, HashMapExt};
use knock_meta::KnockMeta;
use knock_state::KnockState;
use quanta::{Instant, Upkeep};
use slotmap::{DefaultKey, SlotMap};
use socket::RawSocket;
use std::cmp::Reverse;
use std::collections::binary_heap::PeekMut;
use std::collections::hash_map::Entry;
use std::collections::BinaryHeap;
use std::future::{poll_fn, Future};
use std::net::IpAddr;
use std::pin::pin;
use std::task::Poll;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::select;
use tokio::time::{interval, MissedTickBehavior};
use tracing::field::debug;
use tracing::{info, instrument, trace, Level, Span};

type KnockKey = DefaultKey;
type KnockStates = SlotMap<KnockKey, KnockState>;
type ActiveKnocks = HashMap<IpAddr, KnockKey>;
type ExpirationQueue = BinaryHeap<Reverse<(Instant, KnockKey, IpAddr)>>;

fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(Level::INFO)
        .without_time()
        .init();

    let _upkeep = Upkeep::new(Duration::from_millis(5))
        .start()
        .expect("this should be the only call to Upkeep::start");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio should be functional");

    rt.block_on(run());
}

async fn run() {
    // The reason we use a slotmap instead of an Rc<T> here is we want to use its generational indexes
    // so when expiration comes around, we can ensure we don't accidentally expire old data.
    // We are using the Quanta "recent" instant for performance, as we are potentially parsing a huge
    // number of packets per second, and are doing this single-threaded (multithreading here YAGNI)
    //
    // We could theoretically store a copy of the generation separately, but why reinvent the wheel?
    // We get easy deduplication here too.
    //
    // This could potentially be improved memory-wise by interning (or at least reusing) a knock key with
    // more metadata, but realistically I'm not that memory-constrained (IpAddr is 17 bytes for ipv6 + enum discrim)
    //
    // Realistically, I could use something that allows me to remove unnecessary deadlines as I get them
    // as we can change this up trivially to have a more stable key on both sides. But, this is simpler,
    // and likely faster in the performance-sensitive component of parsing headers off the socket,
    // and I'm not *that* memory-constrained.
    //
    // Also ahash is really damn fast, and I'm not all that concerned about collisions here. If you want
    // to replace this with something stronger, you could, but I'm not that concerned with collision overhead here.
    let mut knock_states = KnockStates::new();
    let mut active_knocks = ActiveKnocks::new();
    let mut expiration_queue = ExpirationQueue::new();
    let mut buf = [0u8; 256];

    let mut socket = RawSocket::new().expect("should be able to open a raw socket");

    let mut cleanup_interrupted = false;
    let mut cleanup_interval = pin!(interval(Duration::from_secs(5)));
    cleanup_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        let pending_cleanup_fut = poll_fn(|_cx| {
            if cleanup_interrupted {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });

        select! {
            biased;
            read_bytes = socket.read(&mut buf) => {
                let read_bytes = read_bytes.expect("socket shouldn't be dead");
                handle_knock(
                    &buf[..read_bytes],
                    &mut knock_states,
                    &mut active_knocks,
                    &mut expiration_queue,
                );
            }
            _ = cleanup_interval.tick() => {
                cleanup_interrupted = perform_cleanup(
                    &socket,
                    &mut knock_states,
                    &mut active_knocks,
                    &mut expiration_queue
                ).await;
            }
            _ = pending_cleanup_fut => {
                cleanup_interrupted = perform_cleanup(
                    &socket,
                    &mut knock_states,
                    &mut active_knocks,
                    &mut expiration_queue
                ).await;
            }
        }
    }
}

// If you think this looks like it's overcomplicated, you'd be correct. But, I avoid re-hashing
// wherever possible. You can't stop me, I ain't getting paid to do it right. Lemme have my fun.
#[instrument(skip_all, fields(addr, port))]
fn handle_knock(
    packet: &[u8],
    knock_states: &mut KnockStates,
    active_knocks: &mut ActiveKnocks,
    expiration_queue: &mut ExpirationQueue,
) {
    let Ok(KnockMeta {
        dst_addr: addr,
        dst_port: port,
        ..
    }) = KnockMeta::try_from(packet)
    else {
        return;
    };

    Span::current()
        .record("addr", addr.to_string())
        .record("port", port);

    let knock_entry = active_knocks.entry(addr);

    let knock = match &knock_entry {
        Entry::Vacant(_) => KnockState::try_new(port),
        Entry::Occupied(e) => {
            let knock_key = *e.get();

            // If this panics, you've
            if let KnockState::Passed { .. } = knock_states
                .get(knock_key)
                .expect("active knocks should only contain valid knock states")
            {
                trace!("Door already open");
                return;
            }

            // We must remove the key to invalidate cleanup
            knock_states.remove(knock_key).unwrap().progress(port)
        }
    };

    if !knock.is_valid() {
        if let Entry::Occupied(e) = knock_entry {
            info!("Quiet inside!");
            e.remove();
        }
        return;
    }

    let knock_key = knock_states.insert(knock);
    expiration_queue.push(Reverse((knock.expiration(), knock_key, addr)));

    // Probably a premature optimization. But why re-hash when you don't need to?
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
            info!("Door opened");
        }
        KnockState::Failed => unreachable!("This is never valid by this point"),
    }
}

/// Returns true if cleanup was interrupted by the scheduler
#[instrument(skip_all)]
async fn perform_cleanup(
    socket: &RawSocket,
    knock_states: &mut KnockStates,
    active_knocks: &mut ActiveKnocks,
    expiration_queue: &mut ExpirationQueue,
) -> bool {
    // Cannot use the one from cleanup interval, as we are using quanta's version for performance
    let now = Instant::recent();
    let mut readable = pin!(socket.readable());

    while let Some(item) = expiration_queue.peek_mut() {
        if item.0 .0 > now {
            return false;
        }

        // We have something on our socket - bail out early!
        // This involves mutexes internally, even on a single thread, but is the "safest" way to do this
        // as it handles multiple wakers correctly. Ideally this should be a separate signalling primitive
        // or something like that.
        // But, in the worst case scenario of a huge amount of garbage and a lot of incoming packets,
        // this will allow us to handle new knocks sooner in exchange for filling up with more garbage.
        //
        // This could result in a problematic leak if someone spams our first knock port with a SYN
        // flood, but if that becomes a problem we can just set a hard limit on our garbage amount.
        if poll_fn(|cx| {
            let readable = readable.as_mut().poll(cx);
            Poll::Ready(readable.is_ready())
        })
        .await
        {
            debug("Interrupted by socket becoming readable");
            return true;
        }

        let Reverse((_, knock_key, addr)) = PeekMut::pop(item);

        // We don't want to accidentally clear an expired knock
        let Some(knock) = knock_states.remove(knock_key) else {
            continue;
        };

        active_knocks.remove(&addr);

        if matches!(knock, KnockState::Passed { .. }) {
            info!(%addr, "Door closed");
        } else {
            info!(%addr, "Cleared knock attempts");
        }
    }

    false
}
