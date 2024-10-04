mod knock_meta;
mod knock_state;
mod socket;

use ahash::{HashMap, HashMapExt};
use knock_meta::KnockMeta;
use knock_state::KnockState;
use quanta::{Instant, Upkeep};
use slotmap::{DefaultKey, SlotMap};
use socket::RawSocket;
use std::collections::hash_map::Entry;
use std::collections::BTreeMap;
use std::mem;
use std::net::IpAddr;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::time::interval;
use tokio::{pin, select};
use tracing::{info, trace, Level};

type KnockKey = DefaultKey;

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
    /*
     * The reason we use a slotmap instead of an Rc<T> here is we want to use its generational indexes
     * so when expiration comes around, we can ensure we don't accidentally expire old data.
     * We are using the Quanta "recent" instant for performance, as we are potentially parsing a huge
     * number of packets per second, and are doing this single-threaded (multithreading here YAGNI)
     *
     * We could theoretically store a copy of the generation separately, but why reinvent the wheel?
     * We get easy deduplication here too.
     *
     * This could potentially be improved (without full on interning) by implementing a custom hash
     * and comparison into our KnockState to let us use a HashSet and avoid duplicating IpAddr instances,
     * but I'm not going to lose sleep over wasting 17 bytes here and there for what is a toy (size of IpAddr enum).
     *
     * I'm sure there's many ways to further optimize this, and realistically these things should be
     * properly benchmarked. But I will leave that as an exercise to the reader
     * (which will probably be me in 6 months).
     *
     * As for using a BTreeMap here, it's an easy way to do a queue. Again, realistically, I should
     * use a ring buffer or something. But it's fine. Really. I'm not going to talk myself into
     * spending another 3 days mico-benchmarking this. I swear.
     *
     * Also ahash is fast as fuck, so I'm not all that concerned about collisions here. If you want
     * to replace this with a sparse bitmap or something that'll have theoretically faster access
     * and write times, that's on you. I'm happy with ahash and hashmaps for now.
     */
    let mut knock_states = SlotMap::<KnockKey, KnockState>::new();
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
                trace!("Door already open");
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
            info!("Door opened");
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
                info!("Door closed for {addr}");
            } else {
                info!("Cleared knock attempts for {addr}");
            }
            active_knocks.remove(&addr);
        }
    }
}
