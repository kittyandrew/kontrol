//! Virtual time and the loop that drives it: poll until nothing is ready, then jump the clock to the earliest
//! timer. No wall-clock sleep anywhere. Tasks run under `race`, which gives each its own waker: embassy-net
//! wakes the stack's runner on every socket poll, so one shared waker would re-poll the socket forever.

use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use embassy_time::{Duration, Instant};
use embassy_time_queue_utils::Queue;

struct Clock(Mutex<(u64, Queue)>); // (now in ticks, pending timers)

embassy_time_driver::time_driver_impl!(static CLOCK: Clock = Clock(Mutex::new((0, Queue::new()))));

impl embassy_time_driver::Driver for Clock {
    fn now(&self) -> u64 {
        self.0.lock().unwrap().0
    }

    fn schedule_wake(&self, at: u64, waker: &Waker) {
        let (now, queue) = &mut *self.0.lock().unwrap();
        queue.schedule_wake(at, waker);
        queue.next_expiration(*now); // wakes at once a timer already due
    }
}

/// Jumps to the earliest pending timer and fires it; `false` when no timer is pending.
fn advance() -> bool {
    let (now, queue) = &mut *CLOCK.0.lock().unwrap();
    let next = queue.next_expiration(*now);
    if next == u64::MAX {
        return false;
    }
    *now = next;
    queue.next_expiration(next);
    true
}

static WOKEN: AtomicBool = AtomicBool::new(false);
static POKES: AtomicU64 = AtomicU64::new(0);

struct Root;

impl Wake for Root {
    fn wake(self: Arc<Self>) {
        WOKEN.store(true, Ordering::Relaxed);
    }
}

/// Marks the world changed: every task is polled again before time moves, so each `wait_until` re-checks.
/// Whatever changes state a scenario waits on calls it: the fake ntfy, the wire, the log.
pub fn poke() {
    POKES.fetch_add(1, Ordering::Relaxed);
    WOKEN.store(true, Ordering::Relaxed);
}

/// Polls `fut` to completion on virtual time; panics past `limit` of virtual time or on a deadlock.
pub fn run<F: Future>(fut: F, limit: Duration) -> F::Output {
    let waker = Waker::from(Arc::new(Root));
    let mut cx = Context::from_waker(&waker);
    let mut fut = pin!(fut);
    let mut spins = 0u32;
    loop {
        WOKEN.store(false, Ordering::Relaxed);
        if let Poll::Ready(out) = fut.as_mut().poll(&mut cx) {
            return out;
        }
        if WOKEN.load(Ordering::Relaxed) {
            spins += 1;
            assert!(spins < 100_000, "livelock at {} ms: tasks keep waking without time moving", now_ms());
            continue;
        }
        spins = 0;
        assert!(advance(), "deadlock at {} ms: nothing will ever wake", now_ms());
        assert!(Instant::now().as_ticks() <= limit.as_ticks(), "the scenario ran past its {} s limit", limit.as_secs());
    }
}

pub fn now_ms() -> u64 {
    Instant::now().as_millis()
}

/// Ready once `cond` holds; `poke` is what makes it look again.
pub async fn wait_until(mut cond: impl FnMut() -> bool) {
    poll_fn(|_| if cond() { Poll::Ready(()) } else { Poll::Pending }).await
}

pub type Task<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

struct Child {
    ready: AtomicBool,
    parent: Mutex<Option<Waker>>,
}

impl Wake for Child {
    fn wake(self: Arc<Self>) {
        self.ready.store(true, Ordering::Relaxed);
        if let Some(parent) = &*self.parent.lock().unwrap() {
            parent.wake_by_ref();
        }
    }
}

/// Runs the tasks side by side, each polled only when its own waker fired or after a `poke`, and ends with
/// the first to finish: its index and output. The others are dropped, as a reset drops a board's tasks.
pub async fn race<T>(mut tasks: Vec<Task<'_, T>>) -> (usize, T) {
    let children: Vec<Arc<Child>> =
        tasks.iter().map(|_| Arc::new(Child { ready: AtomicBool::new(true), parent: Mutex::new(None) })).collect();
    let wakers: Vec<Waker> = children.iter().map(|c| Waker::from(c.clone())).collect();
    let mut seen = u64::MAX;
    poll_fn(|cx| {
        let pokes = POKES.load(Ordering::Relaxed);
        let all = pokes != seen;
        seen = pokes;
        for (i, task) in tasks.iter_mut().enumerate() {
            *children[i].parent.lock().unwrap() = Some(cx.waker().clone());
            if (children[i].ready.swap(false, Ordering::Relaxed) || all)
                && let Poll::Ready(out) = task.as_mut().poll(&mut Context::from_waker(&wakers[i]))
            {
                return Poll::Ready((i, out));
            }
        }
        Poll::Pending
    })
    .await
}
